use std::collections::HashMap;
use std::mem::{self, MaybeUninit};

use unsafe_libyaml::{
    yaml_document_delete, yaml_document_get_node, yaml_document_get_root_node, yaml_document_t,
    yaml_mark_t, yaml_node_pair_t, yaml_node_t, yaml_parser_delete, yaml_parser_initialize,
    yaml_parser_load, yaml_parser_set_encoding, yaml_parser_set_input_string, yaml_parser_t,
    YAML_MAPPING_NODE, YAML_SCALAR_NODE, YAML_SEQUENCE_NODE, YAML_UTF8_ENCODING,
};

type ByteSpan = (usize, usize);

#[derive(Debug, Clone, Default)]
pub struct YamlPointerMap {
    spans: HashMap<String, ByteSpan>,
}

impl YamlPointerMap {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parser = LibyamlParser::new(text.as_bytes())?;
        let mut docs = Vec::new();

        loop {
            let mut doc = LibyamlDocument::new();
            if unsafe { yaml_parser_load(parser.as_mut_ptr(), doc.as_mut_ptr()) }.fail {
                return None;
            }
            doc.loaded = true;

            let root = unsafe { yaml_document_get_root_node(doc.as_mut_ptr()) };
            if root.is_null() {
                break;
            }

            let root_id = unsafe { node_id(doc.as_mut_ptr(), root) };
            let mut spans = HashMap::new();
            collect_node_spans(doc.as_mut_ptr(), root_id, "", &mut spans);
            let root_span = spans.get("").copied()?;
            docs.push(DocumentPointerMap { root_span, spans });
        }

        Some(Self {
            spans: merge_document_spans(docs),
        })
    }

    pub fn lookup(&self, pointer: &str) -> Option<ByteSpan> {
        let mut current = pointer;
        loop {
            if let Some(span) = self.spans.get(current) {
                return Some(*span);
            }
            if current.is_empty() {
                return None;
            }
            current = current
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("");
        }
    }
}

#[derive(Debug)]
struct DocumentPointerMap {
    root_span: ByteSpan,
    spans: HashMap<String, ByteSpan>,
}

struct LibyamlParser {
    raw: yaml_parser_t,
}

impl LibyamlParser {
    fn new(input: &[u8]) -> Option<Self> {
        let mut raw = MaybeUninit::<yaml_parser_t>::uninit();
        unsafe {
            if yaml_parser_initialize(raw.as_mut_ptr()).fail {
                return None;
            }
            let mut raw = raw.assume_init();
            yaml_parser_set_encoding(&mut raw, YAML_UTF8_ENCODING);
            yaml_parser_set_input_string(&mut raw, input.as_ptr(), input.len() as u64);
            Some(Self { raw })
        }
    }

    fn as_mut_ptr(&mut self) -> *mut yaml_parser_t {
        &mut self.raw
    }
}

impl Drop for LibyamlParser {
    fn drop(&mut self) {
        unsafe { yaml_parser_delete(&mut self.raw) }
    }
}

struct LibyamlDocument {
    raw: yaml_document_t,
    loaded: bool,
}

impl LibyamlDocument {
    fn new() -> Self {
        Self {
            raw: unsafe { mem::zeroed() },
            loaded: false,
        }
    }

    fn as_mut_ptr(&mut self) -> *mut yaml_document_t {
        &mut self.raw
    }
}

impl Drop for LibyamlDocument {
    fn drop(&mut self) {
        if self.loaded {
            unsafe { yaml_document_delete(&mut self.raw) }
        }
    }
}

fn merge_document_spans(docs: Vec<DocumentPointerMap>) -> HashMap<String, ByteSpan> {
    match docs.len() {
        0 => HashMap::new(),
        1 => docs.into_iter().next().unwrap().spans,
        _ => {
            let mut merged = HashMap::new();
            let start = docs.first().map(|doc| doc.root_span.0).unwrap_or(0);
            let end = docs.last().map(|doc| doc.root_span.1).unwrap_or(start);
            merged.insert(String::new(), (start, end));

            for (index, doc) in docs.into_iter().enumerate() {
                let doc_prefix = format!("/{index}");
                merged.insert(doc_prefix.clone(), doc.root_span);

                for (ptr, span) in doc.spans {
                    if ptr.is_empty() {
                        continue;
                    }
                    merged.insert(format!("{doc_prefix}{ptr}"), span);
                }
            }

            merged
        }
    }
}

fn collect_node_spans(
    document: *mut yaml_document_t,
    node_id: i32,
    pointer: &str,
    spans: &mut HashMap<String, ByteSpan>,
) {
    let node = unsafe { yaml_document_get_node(document, node_id) };
    if node.is_null() {
        return;
    }

    let Some(span) = node_span(node) else {
        return;
    };
    spans.insert(pointer.to_string(), span);

    match unsafe { (*node).type_ } {
        YAML_MAPPING_NODE => collect_mapping_spans(document, node, pointer, spans),
        YAML_SEQUENCE_NODE => collect_sequence_spans(document, node, pointer, spans),
        YAML_SCALAR_NODE => {}
        _ => {}
    }
}

fn collect_mapping_spans(
    document: *mut yaml_document_t,
    node: *mut yaml_node_t,
    pointer: &str,
    spans: &mut HashMap<String, ByteSpan>,
) {
    unsafe {
        let pairs = (*node).data.mapping.pairs;
        let mut pair = pairs.start;
        while pair != pairs.top {
            let yaml_node_pair_t { key, value, .. } = *pair;
            if let Some(key_text) = scalar_node_text(document, key) {
                let child_ptr = join_pointer(pointer, &key_text);
                collect_node_spans(document, value, &child_ptr, spans);
            }
            pair = pair.add(1);
        }
    }
}

fn collect_sequence_spans(
    document: *mut yaml_document_t,
    node: *mut yaml_node_t,
    pointer: &str,
    spans: &mut HashMap<String, ByteSpan>,
) {
    unsafe {
        let items = (*node).data.sequence.items;
        let mut item = items.start;
        let mut index = 0usize;
        while item != items.top {
            let child_ptr = join_pointer(pointer, &index.to_string());
            collect_node_spans(document, *item, &child_ptr, spans);
            index += 1;
            item = item.add(1);
        }
    }
}

fn join_pointer(parent: &str, segment: &str) -> String {
    let mut out = String::with_capacity(parent.len() + segment.len() + 1);
    out.push_str(parent);
    out.push('/');
    out.push_str(&escape_pointer_segment(segment));
    out
}

fn escape_pointer_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for ch in segment.chars() {
        match ch {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            _ => out.push(ch),
        }
    }
    out
}

fn scalar_node_text(document: *mut yaml_document_t, node_id: i32) -> Option<String> {
    let node = unsafe { yaml_document_get_node(document, node_id) };
    if node.is_null() || unsafe { (*node).type_ } != YAML_SCALAR_NODE {
        return None;
    }

    unsafe {
        let scalar = (*node).data.scalar;
        let bytes = std::slice::from_raw_parts(scalar.value, scalar.length as usize);
        std::str::from_utf8(bytes).ok().map(str::to_string)
    }
}

fn node_span(node: *const yaml_node_t) -> Option<ByteSpan> {
    let start = mark_index(unsafe { (*node).start_mark })?;
    let end = mark_index(unsafe { (*node).end_mark })?;
    Some((start, end.max(start)))
}

fn mark_index(mark: yaml_mark_t) -> Option<usize> {
    usize::try_from(mark.index).ok()
}

unsafe fn node_id(document: *mut yaml_document_t, node: *mut yaml_node_t) -> i32 {
    node.offset_from((*document).nodes.start) as i32 + 1
}
