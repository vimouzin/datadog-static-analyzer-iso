// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License, Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2024 Datadog, Inc.

use crate::analysis::ddsa_lib::common::{v8_uint, Class, DDSAJsRuntimeError, Instance, NodeId};
use crate::analysis::ddsa_lib::js::TreeSitterNode;
use crate::analysis::ddsa_lib::v8_ds::MirroredIndexMap;
use crate::analysis::ddsa_lib::{js, RawTSNode};
use crate::analysis::tree_sitter::{TSCaptureContent, TSQueryCapture};
use deno_core::v8;
use deno_core::v8::HandleScope;

/// A stateful bridge holding a collection of [`RawTsNode`].
#[derive(Debug)]
pub struct TsNodeBridge {
    /// NOTE: This MirroredIndexMap has _different_ key/value semantics between Rust and v8:
    ///
    /// In Rust, it is as the type signature indicates: an IndexMap from [`RawTsNode`] to [`NodeId`].
    /// However, in v8, it is the reverse mapping: from [`NodeId`] to [`TreeSitterNode`].
    ///
    /// This allows Rust to send unique TsNodes to v8 (which v8 can quickly look up).
    mirrored_im: MirroredIndexMap<RawTSNode, NodeId>,
    js_class: js::TreeSitterNodeFn<Class>,
}

impl TsNodeBridge {
    /// Constructs a new `TsNodeBridge` for the given `scope`. The scope's [`v8::Context::global`] must
    /// have a class function with the following identifier:
    /// * [`js::TreeSitterNodeFn<Class>::CLASS_NAME`]
    pub fn try_new(scope: &mut HandleScope) -> Result<Self, DDSAJsRuntimeError> {
        let js_class = js::TreeSitterNodeFn::try_new(scope)?;
        let mirrored_im = MirroredIndexMap::<RawTSNode, NodeId>::with_capacity(scope, 128);
        Ok(Self {
            mirrored_im,
            js_class,
        })
    }

    /// Inserts a tree-sitter node into the bridge, returning the `NodeId` it was assigned.
    ///
    /// If the node already existed in the bridge, the existing `NodeId` will be returned.
    pub fn insert(&mut self, scope: &mut HandleScope, node: tree_sitter::Node) -> NodeId {
        let raw_ts_node = RawTSNode::new(node);
        if let Some((_, _, node_id)) = self.mirrored_im.get_full(&raw_ts_node) {
            *node_id
        } else {
            // If the node doesn't exist, upon insertion, its `IndexMap` index will be equivalent
            // to the map's current length. Because we use this as the `NodeId`, we can pre-assign
            // this, knowing this invariant (index == NodeId) will hold.
            let node_id = self.mirrored_im.len() as NodeId;
            let v8_node = self.build_v8_node(scope, node, node_id);
            self.mirrored_im
                .insert_with(scope, raw_ts_node, node_id, |scope, _key, value| {
                    // In Rust, we map from `tree_sitter::Node` to `NodeId`.
                    // Within v8, we want to perform the opposite mapping -- `NodeId` to `tree_sitter::Node`.
                    debug_assert_eq!(node_id, *value);
                    let v8_nid = v8_uint(scope, node_id);
                    (v8_nid.into(), v8_node.into())
                });
            node_id
        }
    }

    /// Retrieves the id of a node within the bridge.
    pub fn get_id(&self, node: tree_sitter::Node) -> Option<NodeId> {
        let raw_ts_node = RawTSNode::new(node);
        self.mirrored_im
            .get_full(&raw_ts_node)
            .map(|(_, _, node_id)| *node_id)
    }

    /// Retrieves a raw tree-sitter node with the assigned `NodeId`.
    pub fn get_raw(&self, node_id: NodeId) -> Option<&RawTSNode> {
        self.mirrored_im
            .get_index(node_id as usize)
            .map(|(raw_node, _)| raw_node)
    }

    /// Serializes a [`tree_sitter::Node`] to v8.
    fn build_v8_node<'s>(
        &self,
        scope: &mut HandleScope<'s>,
        node: tree_sitter::Node,
        id: NodeId,
    ) -> v8::Local<'s, v8::Object> {
        let ts_node = TreeSitterNode::<Instance>::from_ts_node(id, node);
        self.js_class.new_instance(scope, ts_node)
    }

    /// Removes all tree-sitter nodes from the bridge.
    /// Previously allocated `TreeSitterNode` v8 objects will be released to the garbage collector.
    pub fn clear(&mut self, scope: &mut HandleScope) {
        self.mirrored_im.clear(scope);
    }

    /// Returns the number of tree-sitter nodes in the bridge.
    pub fn len(&self) -> usize {
        self.mirrored_im.len()
    }

    /// Returns `true` if the bridge is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a local handle to the underlying [`v8::Global`] map of tree-sitter nodes.
    pub fn as_local<'s>(&self, scope: &mut HandleScope<'s>) -> v8::Local<'s, v8::Map> {
        self.mirrored_im.as_local(scope)
    }

    /// Inserts the nodes from a [`TSQueryCapture<tree_sitter::Node>`] into a v8 scope, consuming the `QueryCapture`.
    /// Returns a transformed `QueryCapture` containing the ids of the inserted nodes.
    pub fn insert_capture(
        &mut self,
        scope: &mut HandleScope,
        capture: TSQueryCapture<tree_sitter::Node>,
    ) -> TSQueryCapture<NodeId> {
        TSQueryCapture::<NodeId> {
            name: capture.name,
            contents: match capture.contents {
                TSCaptureContent::Single(node) => {
                    let nid = self.insert(scope, node);
                    TSCaptureContent::Single(nid)
                }
                TSCaptureContent::Multi(nodes) => {
                    let nids = nodes
                        .into_iter()
                        .map(|node| self.insert(scope, node))
                        .collect::<Vec<_>>();
                    TSCaptureContent::Multi(nids)
                }
            },
        }
    }

    /// Looks up a value in the v8_map, returning the value as a v8 object.
    #[cfg(test)]
    fn v8_get<'s>(
        &self,
        scope: &mut HandleScope<'s>,
        id: NodeId,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let key = v8_uint(scope, id);
        let v8_map = self.mirrored_im.v8_map().open(scope);
        v8_map
            .get(scope, key.into())
            .and_then(|val| v8::Local::<v8::Object>::try_from(val).ok())
    }
}

#[cfg(test)]
mod tests {
    use crate::analysis::ddsa_lib::bridge::ts_node::TsNodeBridge;
    use crate::analysis::ddsa_lib::bridge::ContextBridge;
    use crate::analysis::ddsa_lib::common::get_field;
    use crate::analysis::ddsa_lib::test_utils::{
        attach_as_global, cfg_test_v8, try_execute, TsTree,
    };
    use crate::analysis::ddsa_lib::RawTSNode;
    use crate::model::common::Language;
    use deno_core::v8;
    use deno_core::v8::HandleScope;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    /// Compares whether a [`TreeSitterNodeObj`] has equivalent data to a [`tree_sitter::Node`].
    #[rustfmt::skip]
    fn ts_node_eq(
        scope: &mut HandleScope,
        obj: v8::Local<v8::Object>,
        node: tree_sitter::Node,
    ) -> bool {
        let mut equals = |name: &'static str, other: usize| -> bool {
            get_field::<v8::Integer>(obj, name, scope, "integer").unwrap().value() as usize == other
        };
        // We intentionally do not check `id` here because that is our abstraction, not tree-sitter's.
        // Because we also normalize the `tree_sitter::Point` to be 1-based, we need to do that here.
        equals("_startLine", node.start_position().row + 1)
            && equals("_startCol", node.start_position().column + 1)
            && equals("_endLine", node.end_position().row + 1)
            && equals("_endCol", node.end_position().column + 1)
            && equals("_typeId", node.grammar_id() as usize)
    }

    fn setup_bridge() -> (deno_core::JsRuntime, Rc<RefCell<TsNodeBridge>>) {
        let mut runtime = cfg_test_v8().deno_core_rt();
        let bridge = TsNodeBridge::try_new(&mut runtime.handle_scope()).unwrap();
        let bridge = Rc::new(RefCell::new(bridge));
        runtime.op_state().borrow_mut().put(Rc::clone(&bridge));
        (runtime, bridge)
    }

    fn setup_context_bridge(runtime: &mut deno_core::JsRuntime) -> Rc<RefCell<ContextBridge>> {
        let bridge = ContextBridge::try_new(&mut runtime.handle_scope()).unwrap();
        let bridge = Rc::new(RefCell::new(bridge));
        runtime.op_state().borrow_mut().put(Rc::clone(&bridge));
        bridge
    }

    /// Nodes can be inserted from Rust, and they can be retrieved within v8 from their `NodeId`.
    #[test]
    fn ts_node_bridge_is_synced() {
        let (mut runtime, bridge) = setup_bridge();
        let scope = &mut runtime.handle_scope();
        let mut bridge = bridge.borrow_mut();

        let tree = TsTree::new(r#"const val = foo(bar, baz);"#, Language::JavaScript);
        let foo = tree.find_named_nodes(Some("foo"), None)[0];
        let bar = tree.find_named_nodes(Some("bar"), None)[0];
        let baz = tree.find_named_nodes(Some("baz"), None)[0];

        assert!(bridge.v8_get(scope, 0).is_none());
        assert_eq!(bridge.insert(scope, foo), 0);
        let v8_tsn = bridge.v8_get(scope, 0).unwrap();
        assert!(ts_node_eq(scope, v8_tsn, foo));
        assert!(bridge.v8_get(scope, 1).is_none());
        assert_eq!(bridge.insert(scope, bar), 1);
        assert_eq!(bridge.insert(scope, baz), 2);

        bridge.clear(scope);
        assert!(bridge.is_empty());
        assert_eq!(bridge.insert(scope, baz), 0);
        let v8_tsn = bridge.v8_get(scope, 0).unwrap();
        assert!(ts_node_eq(scope, v8_tsn, baz));
    }

    /// We can look up tree-sitter nodes by id, or ids by hash.
    #[test]
    fn ts_node_rust_lookup() {
        let (mut runtime, bridge) = setup_bridge();
        let scope = &mut runtime.handle_scope();
        let mut bridge = bridge.borrow_mut();

        let tree = TsTree::new(r#"const val = foo(bar, baz);"#, Language::JavaScript);
        let foo = tree.find_named_nodes(Some("foo"), None)[0];
        let bar = tree.find_named_nodes(Some("bar"), None)[0];
        let baz = tree.find_named_nodes(Some("baz"), None)[0];

        for node in [foo, bar, baz] {
            bridge.insert(scope, node);
        }

        let bar_raw = RawTSNode::new(bar);
        assert_eq!(bridge.get_id(bar).unwrap(), 1);
        assert_eq!(bridge.get_raw(1).unwrap(), &bar_raw);
    }

    #[test]
    fn ts_node_bridge_no_duplicates() {
        let (mut runtime, bridge) = setup_bridge();
        let scope = &mut runtime.handle_scope();
        let mut bridge = bridge.borrow_mut();

        let tree = TsTree::new(r#"const val = foo(bar, baz);"#, Language::JavaScript);
        let foo = tree.find_named_nodes(Some("foo"), None)[0];

        assert_eq!(bridge.insert(scope, foo), 0);
        assert!(bridge.v8_get(scope, 0).is_some());
        assert!(bridge.v8_get(scope, 1).is_none());
        assert_eq!(bridge.insert(scope, foo), 0);
        assert!(bridge.v8_get(scope, 1).is_none());
    }

    /// The text that the node spans can be retrieved.
    #[test]
    fn get_node_text() {
        let (mut runtime, ts_node_bridge) = setup_bridge();
        let file_contents = "\
const abc = 123;
const def = 456;
";
        let tree = TsTree::new(file_contents, Language::JavaScript);
        let file_contents = Arc::<str>::from(file_contents);
        let file_name = Arc::<str>::from("file_name.js");

        // The provider of the text is the "context", so we need to create and populate that first.
        let ctx_bridge = setup_context_bridge(&mut runtime);
        let scope = &mut runtime.handle_scope();
        let ts_node_map = ts_node_bridge.borrow().as_local(scope);
        attach_as_global(scope, ts_node_map, "TS_NODES");
        ctx_bridge
            .borrow_mut()
            .set_root_context(scope, &tree.tree(), &file_contents, &file_name);
        let node_0 = tree.find_named_nodes(Some("abc"), Some("identifier"))[0];
        let node_1 = tree.find_named_nodes(Some("456"), Some("number"))[0];
        ts_node_bridge.borrow_mut().insert(scope, node_0);
        ts_node_bridge.borrow_mut().insert(scope, node_1);
        let expected = [(0, "abc"), (1, "456")]
            .map(|(nid, text)| (format!("TS_NODES.get({}).text;", nid), text));
        for (code, text) in &expected {
            let res = try_execute(scope, code).unwrap();
            assert_eq!(res.to_rust_string_lossy(scope).as_str(), *text);
        }
        // Test the caching behavior by mutating the `file_contents` so that an op call would
        // return different text (i.e. a second op call would fail the test).
        let dummy_contents = Arc::<str>::from("Z".repeat(file_contents.len()));
        ctx_bridge
            .borrow_mut()
            .set_root_context(scope, &tree.tree(), &dummy_contents, &file_name);
        for (code, text) in &expected {
            let res = try_execute(scope, code).unwrap();
            assert_eq!(res.to_rust_string_lossy(scope).as_str(), *text);
        }
    }

    /// Tests that if a rule mutates the internal node id (which it "should" never do), calling
    /// the text getter returns an empty string instead of panicking.
    #[test]
    fn get_invalid_node_text() {
        let (mut runtime, ts_node_bridge) = setup_bridge();
        let file_contents = "const abc = 123;";
        let tree = TsTree::new(file_contents, Language::JavaScript);
        let file_contents = Arc::<str>::from(file_contents);
        let file_name = Arc::<str>::from("file_name.js");

        // The provider of the text is the "context", so we need to create and populate that first.
        let ctx_bridge = setup_context_bridge(&mut runtime);
        let scope = &mut runtime.handle_scope();
        let ts_node_map = ts_node_bridge.borrow().as_local(scope);
        attach_as_global(scope, ts_node_map, "TS_NODES");
        ctx_bridge
            .borrow_mut()
            .set_root_context(scope, &tree.tree(), &file_contents, &file_name);
        let node_0 = tree.find_named_nodes(Some("abc"), Some("identifier"))[0];
        ts_node_bridge.borrow_mut().insert(scope, node_0);

        let code = "\
const node = TS_NODES.get(0);
node.id = 123456789;
node.text;
";
        let value = try_execute(scope, code).unwrap();
        assert!(value.is_undefined());
    }
}
