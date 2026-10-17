//! Arena-backed DOM tree.
//!
//! Nodes live in a `Vec` and are addressed by `NodeId`. Removed nodes stay in
//! the arena (detached) so that ids held by scripts remain valid.

pub type NodeId = usize;

#[derive(Debug, Clone)]
pub struct ElementData {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum NodeData {
    Document,
    Fragment,
    Element(ElementData),
    Text(String),
    Comment(String),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub nodes: Vec<Node>,
    /// Bumped on every mutation; used to invalidate style/layout caches.
    pub version: u64,
}

pub const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Document {
            nodes: vec![Node { parent: None, children: vec![], data: NodeData::Document }],
            version: 0,
        }
    }

    pub fn root(&self) -> NodeId {
        0
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node { parent: None, children: vec![], data });
        self.version += 1;
        self.nodes.len() - 1
    }

    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.push(NodeData::Element(ElementData { tag: tag.to_ascii_lowercase(), attrs: vec![] }))
    }

    pub fn create_element_with_attrs(&mut self, tag: &str, attrs: Vec<(String, String)>) -> NodeId {
        self.push(NodeData::Element(ElementData { tag: tag.to_ascii_lowercase(), attrs }))
    }

    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.push(NodeData::Text(text.to_string()))
    }

    pub fn create_comment(&mut self, text: &str) -> NodeId {
        self.push(NodeData::Comment(text.to_string()))
    }

    pub fn create_fragment(&mut self) -> NodeId {
        self.push(NodeData::Fragment)
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id).and_then(|n| n.parent)
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match &self.nodes.get(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match &mut self.nodes.get_mut(id)?.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        self.element(id).is_some()
    }

    pub fn tag(&self, id: NodeId) -> Option<&str> {
        self.element(id).map(|e| e.tag.as_str())
    }

    pub fn tag_is(&self, id: NodeId, tag: &str) -> bool {
        self.tag(id) == Some(tag)
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id)?.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn has_attr(&self, id: NodeId, name: &str) -> bool {
        self.attr(id, name).is_some()
    }

    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        let name = name.to_ascii_lowercase();
        if let Some(el) = self.element_mut(id) {
            if let Some(slot) = el.attrs.iter_mut().find(|(k, _)| *k == name) {
                if slot.1 == value {
                    return;
                }
                slot.1 = value.to_string();
            } else {
                el.attrs.push((name, value.to_string()));
            }
            self.version += 1;
        }
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        if let Some(el) = self.element_mut(id) {
            let before = el.attrs.len();
            el.attrs.retain(|(k, _)| k != name);
            if el.attrs.len() != before {
                self.version += 1;
            }
        }
    }

    pub fn classes(&self, id: NodeId) -> Vec<&str> {
        self.attr(id, "class").map(|c| c.split_ascii_whitespace().collect()).unwrap_or_default()
    }

    pub fn has_class(&self, id: NodeId, class: &str) -> bool {
        self.attr(id, "class").is_some_and(|c| c.split_ascii_whitespace().any(|x| x == class))
    }

    pub fn is_ancestor(&self, ancestor: NodeId, mut node: NodeId) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            match self.parent(node) {
                Some(p) => node = p,
                None => return false,
            }
        }
    }

    /// Detach `child` from its current parent (if any).
    pub fn detach(&mut self, child: NodeId) {
        if let Some(p) = self.nodes[child].parent.take() {
            self.nodes[p].children.retain(|&c| c != child);
            self.version += 1;
        }
    }

    /// Append `child` to `parent`. Fragments are unpacked.
    /// Returns false if the operation would create a cycle.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        self.insert_before(parent, child, None)
    }

    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> bool {
        if self.is_ancestor(child, parent) {
            return false;
        }
        if matches!(self.nodes[child].data, NodeData::Fragment) {
            let kids = std::mem::take(&mut self.nodes[child].children);
            for k in kids {
                self.nodes[k].parent = None;
                self.insert_before(parent, k, before);
            }
            return true;
        }
        self.detach(child);
        let idx = before
            .and_then(|b| self.nodes[parent].children.iter().position(|&c| c == b))
            .unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(idx, child);
        self.nodes[child].parent = Some(parent);
        self.version += 1;
        true
    }

    pub fn remove_all_children(&mut self, parent: NodeId) {
        let kids = std::mem::take(&mut self.nodes[parent].children);
        for k in kids {
            self.nodes[k].parent = None;
        }
        self.version += 1;
    }

    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Text(t) => out.push_str(t),
            NodeData::Comment(_) => {}
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out);
                }
            }
        }
    }

    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        match &mut self.nodes[id].data {
            NodeData::Text(t) | NodeData::Comment(t) => {
                *t = text.to_string();
                self.version += 1;
            }
            _ => {
                self.remove_all_children(id);
                if !text.is_empty() {
                    let t = self.create_text(text);
                    self.append_child(id, t);
                }
            }
        }
    }

    pub fn set_text(&mut self, id: NodeId, text: &str) {
        if let NodeData::Text(t) = &mut self.nodes[id].data {
            *t = text.to_string();
            self.version += 1;
        }
    }

    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.nodes[id].children.iter().copied().filter(|&c| self.is_element(c)).collect()
    }

    pub fn document_element(&self) -> Option<NodeId> {
        self.element_children(0).into_iter().next()
    }

    pub fn find_child_tag(&self, parent: NodeId, tag: &str) -> Option<NodeId> {
        self.nodes[parent].children.iter().copied().find(|&c| self.tag_is(c, tag))
    }

    pub fn body(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.find_child_tag(html, "body")
    }

    pub fn head(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.find_child_tag(html, "head")
    }

    /// Pre-order traversal of all descendants (excluding `root`).
    pub fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[root].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev().copied());
        }
        out
    }

    pub fn element_descendants(&self, root: NodeId) -> Vec<NodeId> {
        self.descendants(root).into_iter().filter(|&n| self.is_element(n)).collect()
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.element_descendants(0).into_iter().find(|&n| self.attr(n, "id") == Some(id))
    }

    pub fn elements_by_tag(&self, root: NodeId, tag: &str) -> Vec<NodeId> {
        let tag = tag.to_ascii_lowercase();
        self.element_descendants(root).into_iter().filter(|&n| tag == "*" || self.tag_is(n, &tag)).collect()
    }

    pub fn title(&self) -> String {
        self.elements_by_tag(0, "title")
            .first()
            .map(|&t| collapse_ws(&self.text_content(t)))
            .unwrap_or_default()
    }

    pub fn index_in_parent(&self, id: NodeId) -> Option<usize> {
        let p = self.parent(id)?;
        self.nodes[p].children.iter().position(|&c| c == id)
    }

    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        let p = self.parent(id)?;
        let i = self.index_in_parent(id)?;
        self.nodes[p].children.get(i + 1).copied()
    }

    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        let p = self.parent(id)?;
        let i = self.index_in_parent(id)?;
        if i == 0 { None } else { self.nodes[p].children.get(i - 1).copied() }
    }

    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.next_sibling(id);
        while let Some(c) = cur {
            if self.is_element(c) {
                return Some(c);
            }
            cur = self.next_sibling(c);
        }
        None
    }

    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = self.prev_sibling(id);
        while let Some(c) = cur {
            if self.is_element(c) {
                return Some(c);
            }
            cur = self.prev_sibling(c);
        }
        None
    }

    /// Deep or shallow clone; the clone is detached.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        let data = self.nodes[id].data.clone();
        let new = self.push(data);
        if deep {
            let kids = self.nodes[id].children.clone();
            for k in kids {
                let c = self.clone_node(k, true);
                self.append_child(new, c);
            }
        }
        new
    }

    pub fn is_connected(&self, id: NodeId) -> bool {
        self.is_ancestor(0, id)
    }

    // ---- serialization ----

    pub fn inner_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        for &c in &self.nodes[id].children {
            self.serialize(c, &mut out);
        }
        out
    }

    pub fn outer_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.serialize(id, &mut out);
        out
    }

    fn serialize(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Text(t) => {
                let raw = self
                    .parent(id)
                    .and_then(|p| self.tag(p))
                    .is_some_and(|t| matches!(t, "script" | "style"));
                if raw { out.push_str(t) } else { out.push_str(&escape_text(t)) }
            }
            NodeData::Comment(t) => {
                out.push_str("<!--");
                out.push_str(t);
                out.push_str("-->");
            }
            NodeData::Element(e) => {
                out.push('<');
                out.push_str(&e.tag);
                for (k, v) in &e.attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&escape_attr(v));
                    out.push('"');
                }
                out.push('>');
                if VOID_ELEMENTS.contains(&e.tag.as_str()) {
                    return;
                }
                for &c in &self.nodes[id].children {
                    self.serialize(c, out);
                }
                out.push_str("</");
                out.push_str(&e.tag);
                out.push('>');
            }
            NodeData::Document | NodeData::Fragment => {
                for &c in &self.nodes[id].children {
                    self.serialize(c, out);
                }
            }
        }
    }

    /// Debug dump used by `--dump-dom`.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_node(0, 0, &mut out);
        out
    }

    fn dump_node(&self, id: NodeId, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        match &self.nodes[id].data {
            NodeData::Document => out.push_str("#document\n"),
            NodeData::Fragment => out.push_str(&format!("{indent}#fragment\n")),
            NodeData::Element(e) => {
                out.push_str(&format!("{indent}<{}", e.tag));
                for (k, v) in &e.attrs {
                    out.push_str(&format!(" {k}=\"{v}\""));
                }
                out.push_str(">\n");
            }
            NodeData::Text(t) => {
                if t.trim().is_empty() {
                    return;
                }
                out.push_str(&format!("{indent}\"{}\"\n", t.trim()));
            }
            NodeData::Comment(t) => out.push_str(&format!("{indent}<!--{t}-->\n")),
        }
        for &c in &self.nodes[id].children {
            self.dump_node(c, depth + 1, out);
        }
    }
}

pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\u{a0}', "&nbsp;")
}

pub fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
