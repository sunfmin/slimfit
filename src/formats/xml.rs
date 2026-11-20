//! A small event walk over XML: element starts (with attributes), text, and
//! ends, so each format's reader is a fold over these.

use anyhow::Result;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::collections::HashMap;

pub enum Node {
    Open { name: String, attrs: HashMap<String, String> },
    Text(String),
    Close(String),
}

fn attrs(e: &BytesStart) -> HashMap<String, String> {
    e.attributes()
        .filter_map(|a| a.ok())
        .map(|a| {
            let key = a.key.local_name().into_inner().to_string();
            let value = a.normalized_value(XmlVersion::default()).map(|v| v.into_owned()).unwrap_or_default();
            (key, value)
        })
        .collect()
}

pub fn walk(text: &str, mut f: impl FnMut(Node)) -> Result<()> {
    let mut reader = Reader::from_str(text);
    // Blank text right after a reference is content (`&lt; &gt;`); elsewhere
    // it is only indentation.
    let mut after_ref = false;
    loop {
        let event = reader.read_event()?;
        let was_after_ref = std::mem::replace(&mut after_ref, matches!(event, Event::GeneralRef(_)));
        match event {
            Event::Start(e) => f(Node::Open { name: e.local_name().into_inner().to_string(), attrs: attrs(&e) }),
            Event::Empty(e) => {
                let name = e.local_name().into_inner().to_string();
                f(Node::Open { name: name.clone(), attrs: attrs(&e) });
                f(Node::Close(name));
            }
            Event::Text(t) => {
                let s = t.xml_content(XmlVersion::default());
                if was_after_ref || !s.trim().is_empty() {
                    f(Node::Text(s.into_owned()));
                }
            }
            Event::CData(t) => f(Node::Text(t.as_ref().to_string())),
            Event::GeneralRef(r) => {
                let s = match r.as_ref() {
                    "lt" => "<".to_string(),
                    "gt" => ">".to_string(),
                    "amp" => "&".to_string(),
                    "quot" => "\"".to_string(),
                    "apos" => "'".to_string(),
                    _ => r.resolve_char_ref().ok().flatten().map(String::from).unwrap_or_default(),
                };
                f(Node::Text(s))
            }
            Event::End(e) => f(Node::Close(e.local_name().into_inner().to_string())),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}
