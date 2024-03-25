// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License, Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2024 Datadog, Inc.

use crate::rule::RuleId;
use crate::validator::{Candidate, SecretCategory, Validator, ValidatorError, ValidatorId};
use governor::clock::Clock;
use governor::middleware::NoOpMiddleware;
use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::ops::Add;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpValidatorError {
    /// The validation hit an unrecoverable error, and no further attempts will be made.
    #[error("unrecoverable validation failure")]
    RequestedAbort(Box<Result<ureq::Response, ureq::Error>>),
    #[error("invalid url: `{0}` ({1})")]
    InvalidUrl(String, url::ParseError),
    #[error("unsupported HTTP method `{0}`")]
    InvalidMethod(String),
    #[error("validation attempt exceeded the time limit")]
    RetryTimeExceeded { attempted: usize, elapsed: Duration },
    #[error("all validation retry attempts used")]
    RetryAttemptsExceeded { attempted: usize, elapsed: Duration },
    #[error("validation retry will exceed the overall time limit")]
    RetryWillExceedTime {
        attempted: usize,
        elapsed: Duration,
        next_delay: Duration,
    },
    #[error("no qualifying handler that matches the server response")]
    UnhandledResponse(Box<Result<ureq::Response, ureq::Error>>),
}

/// The configuration for re-attempting failed HTTP requests.
pub(crate) struct RetryConfig {
    max_attempts: usize,
    use_jitter: bool,
    policy: RetryPolicy,
}

pub(crate) enum RetryPolicy {
    Exponential {
        base: Duration,
        factor: f64,
        maximum: Duration,
    },
    Fixed {
        duration: Duration,
    },
}

type DynFnResponseParser = dyn Fn(&Result<ureq::Response, ureq::Error>) -> NextAction;

type RateLimiter<T> = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    T,
    NoOpMiddleware<<T as Clock>::Instant>,
>;

pub struct HttpValidator<T: Clock> {
    validator_id: ValidatorId,
    /// The maximum time allowed for a single validation attempt, inclusive of rate-limiting and retry delay.
    max_attempt_duration: Duration,
    rule_id: RuleId,
    attempted_cache: Arc<Mutex<HashSet<[u8; 32]>>>,
    clock: T,
    rate_limiter: Arc<RateLimiter<T>>,
    request_generator: RequestGenerator,
    response_parser: Box<DynFnResponseParser>,
    retry_timings_iter: Box<dyn Fn() -> Box<dyn Iterator<Item = Duration>>>,
}

/// The next action to take after an HTTP request has received a response.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum NextAction {
    /// The validation should immediately be halted, and no further retries should be attempted.
    Abort,
    /// The handler indicated that the validation should be retried.
    Retry,
    /// The handler indicated that the validation should be retried, and gave a specific time to re-attempt.
    RetryAfter(Duration),
    /// The handler successfully performed a validation and categorized the candidate.
    ReturnResult(SecretCategory),
    /// No registered handler could handle the HTTP response result, so a default fallback error was generated.
    Unhandled,
}

#[allow(clippy::type_complexity)]
struct RequestGenerator {
    agent: ureq::Agent,
    method: HttpMethod,
    format_url: Box<dyn Fn(&Candidate) -> String>,
    add_headers: Box<dyn Fn(&Candidate, &mut ureq::Request)>,
    build_post_payload: Option<Box<dyn Fn(&Candidate) -> Vec<u8>>>,
}

impl<T: Clock> Validator for HttpValidator<T> {
    fn id(&self) -> &ValidatorId {
        &self.validator_id
    }

    fn validate(&self, candidate: Candidate) -> Result<SecretCategory, ValidatorError> {
        let start_time = Instant::now();

        let retry_delays = (self.retry_timings_iter)();
        let mut iter = retry_delays.peekable();
        let mut attempted = 0;

        while let Some(retry_delay) = iter.next() {
            // Certain branches can add to the required sleep time, so we track this as a mutable variable.
            let mut to_sleep = retry_delay;

            loop {
                let elapsed = start_time.elapsed();
                if elapsed > self.max_attempt_duration {
                    return Err(HttpValidatorError::RetryTimeExceeded { attempted, elapsed }.into());
                }
                match self.rate_limiter.check() {
                    Ok(_) => break,
                    Err(try_again_at) => {
                        let next_delay = try_again_at.wait_time_from(self.clock.now());
                        let elapsed = start_time.elapsed();
                        if elapsed.add(next_delay) > self.max_attempt_duration {
                            return Err(HttpValidatorError::RetryWillExceedTime {
                                attempted,
                                elapsed,
                                next_delay,
                            }
                            .into());
                        }
                        thread::sleep(next_delay);
                    }
                }
            }

            let formatted_url = (self.request_generator.format_url)(&candidate);

            let url = Url::parse(&formatted_url)
                .map_err(|parse_err| HttpValidatorError::InvalidUrl(formatted_url, parse_err))?;

            let mut request = self
                .request_generator
                .agent
                .request(self.request_generator.method.as_ref(), url.as_str());
            (self.request_generator.add_headers)(&candidate, &mut request);

            attempted += 1;
            let response = match &self.request_generator.method {
                HttpMethod::Get => request.call(),
                HttpMethod::Post => {
                    let bytes_payload = self
                        .request_generator
                        .build_post_payload
                        .as_ref()
                        .map(|get_payload_for| get_payload_for(&candidate))
                        .unwrap_or_default();
                    request.send_bytes(&bytes_payload)
                }
            };

            let next_action = (self.response_parser)(&response);

            match next_action {
                NextAction::Abort => {
                    return Err(HttpValidatorError::RequestedAbort(Box::new(response)).into());
                }
                NextAction::Retry => {}
                NextAction::RetryAfter(http_retry_after) => {
                    // Calculate the amount to sleep based on what we know our next sleep duration will be.
                    // For example, if the `Retry-After` is 15 seconds, and our next sleep will be 10 seconds,
                    // add an additional 5 seconds. If our next sleep is 20 seconds, add 0 seconds.
                    to_sleep += http_retry_after
                        .checked_sub(iter.peek().copied().unwrap_or_default())
                        .unwrap_or_default();
                }
                NextAction::ReturnResult(result) => return Ok(result),
                NextAction::Unhandled => {
                    return Err(HttpValidatorError::UnhandledResponse(Box::new(response)).into());
                }
            }

            // Only sleep if this isn't the last attempt
            if iter.peek().is_some() {
                let elapsed = start_time.elapsed();
                if (elapsed + to_sleep) >= self.max_attempt_duration {
                    return Err(HttpValidatorError::RetryWillExceedTime {
                        attempted,
                        elapsed,
                        next_delay: to_sleep,
                    }
                    .into());
                }
                thread::sleep(to_sleep);
            }
        }

        // We're within our time budget but exhausted our retry budget
        Err(HttpValidatorError::RetryAttemptsExceeded {
            attempted,
            elapsed: start_time.elapsed(),
        }
        .into())
    }
}

impl From<HttpValidatorError> for ValidatorError {
    fn from(value: HttpValidatorError) -> Self {
        Self::ChildError {
            validator_type: "http".to_string(),
            err: Box::new(value),
        }
    }
}

/// The supported HTTP methods that can be used with a request.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
enum HttpMethod {
    Get,
    Post,
}

impl AsRef<str> for HttpMethod {
    fn as_ref(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
        }
    }
}

impl TryFrom<&str> for HttpMethod {
    type Error = HttpValidatorError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Ok(match value {
            "GET" => Self::Get,
            "POST" => Self::Post,
            _ => Err(HttpValidatorError::InvalidMethod(value.to_string()))?,
        })
    }
}

impl Display for HttpMethod {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_ref())
    }
}
