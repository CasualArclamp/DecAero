// made by CasualArclamp
//! The menu an SDR++ server hands its client: the radio's own controls
//! (sample rate, gains, bias-T and so on), which the client draws and sends
//! changes back from.
//!
//! The layout is SDR++'s `SmGui` (core/src/gui/smgui.cpp, GPL-3): the server
//! records its menu as a list of items, a draw step followed by its
//! arguments (`DrawList::storeItem`), and an action is the widget's label
//! and its new value (`Client::showMenu` in sdrpp_server_client.cpp).

/// One thing to draw.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    /// The name of the control that follows.
    Label(String),
    Text(String),
    /// What follows (until the next) cannot be changed just now.
    Disabled(bool),
    Combo {
        id: String,
        value: i32,
        items: Vec<String>,
    },
    Button {
        id: String,
    },
    Radio {
        id: String,
        on: bool,
    },
    SliderInt {
        id: String,
        value: i32,
        min: i32,
        max: i32,
    },
    /// `step` is 0 for a smooth one.
    SliderFloat {
        id: String,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
    },
    InputInt {
        id: String,
        value: i32,
    },
    Checkbox {
        id: String,
        value: bool,
    },
    InputText {
        id: String,
        value: String,
    },
}

/// A widget's new value, sent back to the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A button or a radio button pressed.
    Pressed,
    Bool(bool),
    Int(i32),
    Float(f32),
    Text(String),
}

/// What is shown of a widget's label: SDR++ (as Dear ImGui) hides whatever
/// follows `##`, which is only there to tell widgets apart.
pub fn shown(id: &str) -> &str {
    id.split("##").next().unwrap_or(id)
}

#[derive(Debug, Clone, PartialEq)]
enum Item {
    Step(u8),
    Bool(bool),
    Int(i32),
    Float(f32),
    Text(String),
}

// `SmGui::DrawListElemType`.
const ELEM_STEP: u8 = 0;
const ELEM_BOOL: u8 = 1;
const ELEM_INT: u8 = 2;
const ELEM_FLOAT: u8 = 3;
const ELEM_STRING: u8 = 4;

/// `DrawList::load`: `None` if the bytes are not a list of items.
fn items(mut data: &[u8]) -> Option<Vec<Item>> {
    let mut out = Vec::new();
    while let Some((&kind, rest)) = data.split_first() {
        let (item, used) = match kind {
            ELEM_STEP => (Item::Step(*rest.first()?), 2),
            ELEM_BOOL => (Item::Bool(*rest.first()? != 0), 1),
            ELEM_INT => (
                Item::Int(i32::from_le_bytes(rest.get(..4)?.try_into().ok()?)),
                4,
            ),
            ELEM_FLOAT => (
                Item::Float(f32::from_le_bytes(rest.get(..4)?.try_into().ok()?)),
                4,
            ),
            ELEM_STRING => {
                let len = usize::from(u16::from_le_bytes(rest.get(..2)?.try_into().ok()?));
                let text = String::from_utf8_lossy(rest.get(2..2 + len)?).into_owned();
                (Item::Text(text), 2 + len)
            }
            _ => return None,
        };
        out.push(item);
        data = rest.get(used..)?;
    }
    Some(out)
}

/// The widgets of a menu as the server sent it (the body of its answer to
/// `COMMAND_GET_UI` or `COMMAND_UI_ACTION`). Layout steps (columns, groups,
/// tables, widths) are left out: the client lays the controls out itself.
pub fn parse(data: &[u8]) -> Option<Vec<Widget>> {
    use Item::{Bool, Float, Int, Text};
    let items = items(data)?;
    let mut out = Vec::new();
    let mut in_popup = false;
    let mut i = 0;
    while i < items.len() {
        let Item::Step(step) = items[i] else {
            i += 1;
            continue;
        };
        // The step's arguments: everything up to the next step.
        let end = items[i + 1..]
            .iter()
            .position(|x| matches!(x, Item::Step(_)))
            .map_or(items.len(), |n| i + 1 + n);
        let args = &items[i + 1..end];
        i = end;
        // `SmGui::DrawStep`.
        let widget = match (step, args) {
            (0x02, _) => Some(Widget::Disabled(true)),
            (0x03, _) => Some(Widget::Disabled(false)),
            (0x80, [Text(id), Int(value), Text(list), Int(_)]) => Some(Widget::Combo {
                id: id.clone(),
                value: *value,
                items: list
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
            }),
            (0x81, [Text(id), Float(_), Float(_)]) => Some(Widget::Button { id: id.clone() }),
            (0x84, [Text(id), Bool(on)]) => Some(Widget::Radio {
                id: id.clone(),
                on: *on,
            }),
            (0x87, [Text(text)]) => Some(Widget::Label(text.clone())),
            (0x88, [Text(id), Int(value), Int(min), Int(max), Int(_), Int(_)]) => {
                Some(Widget::SliderInt {
                    id: id.clone(),
                    value: *value,
                    min: *min,
                    max: *max,
                })
            }
            (
                0x89,
                [
                    Text(id),
                    Float(value),
                    Float(min),
                    Float(max),
                    Float(step),
                    Int(_),
                ],
            ) => Some(Widget::SliderFloat {
                id: id.clone(),
                value: *value,
                min: *min,
                max: *max,
                step: *step,
            }),
            (0x8A, [Text(id), Int(value), Int(_), Int(_), Int(_)]) => Some(Widget::InputInt {
                id: id.clone(),
                value: *value,
            }),
            (0x8B, [Text(id), Bool(value)]) => Some(Widget::Checkbox {
                id: id.clone(),
                value: *value,
            }),
            (
                0x8C,
                [
                    Text(id),
                    Float(value),
                    Float(min),
                    Float(max),
                    Int(_),
                    Int(_),
                ],
            ) => Some(Widget::SliderFloat {
                id: id.clone(),
                value: *value,
                min: *min,
                max: *max,
                step: 0.0,
            }),
            (0x8D, [Text(id), Text(value), Int(_), Int(_)]) => Some(Widget::InputText {
                id: id.clone(),
                value: value.clone(),
            }),
            (0x8E, [Text(text)]) => Some(Widget::Text(text.clone())),
            (0x8F, [Float(_), Float(_), Float(_), Float(_), Text(text)]) => {
                Some(Widget::Text(text.clone()))
            }
            // A pop-up is only drawn when the server opens it: left out.
            (0x91, _) => {
                in_popup = true;
                None
            }
            (0x92, _) => {
                in_popup = false;
                None
            }
            _ => None,
        };
        if let Some(w) = widget
            && !in_popup
        {
            out.push(w);
        }
    }
    Some(out)
}

/// `DrawList::storeItem` for a string.
fn store_text(out: &mut Vec<u8>, text: &str) {
    let bytes = &text.as_bytes()[..text.len().min(usize::from(u16::MAX))];
    out.push(ELEM_STRING);
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// The data of a `COMMAND_UI_ACTION`: widget `id` now has `value`. The
/// server is asked to send its menu back, as it stands after the change.
pub fn action(id: &str, value: &Value) -> Vec<u8> {
    let mut out = vec![1u8];
    store_text(&mut out, id);
    match value {
        // SDR++ sends a bool it never sets for these.
        Value::Pressed => out.extend_from_slice(&[ELEM_BOOL, 0]),
        Value::Bool(b) => out.extend_from_slice(&[ELEM_BOOL, u8::from(*b)]),
        Value::Int(i) => {
            out.push(ELEM_INT);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Float(f) => {
            out.push(ELEM_FLOAT);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Text(t) => store_text(&mut out, t),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(out: &mut Vec<u8>, code: u8) {
        out.extend_from_slice(&[ELEM_STEP, code, 0]);
    }
    fn int(out: &mut Vec<u8>, v: i32) {
        out.push(ELEM_INT);
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn float(out: &mut Vec<u8>, v: f32) {
        out.push(ELEM_FLOAT);
        out.extend_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn a_menu_is_read() {
        // As a source module records it: a rate combo across the width, a
        // labelled gain slider, a check box, and a table (laid out here).
        let mut m = Vec::new();
        step(&mut m, 0x00);
        step(&mut m, 0x80);
        store_text(&mut m, "##_rate");
        int(&mut m, 1);
        store_text(&mut m, "2.5MSPS\x0010MSPS\0");
        int(&mut m, -1);
        step(&mut m, 0x87);
        store_text(&mut m, "LNA Gain");
        step(&mut m, 0x00);
        step(&mut m, 0x88);
        store_text(&mut m, "##_lna");
        for v in [7, 0, 15, 1, 0] {
            int(&mut m, v);
        }
        step(&mut m, 0x02);
        step(&mut m, 0x8B);
        store_text(&mut m, "Bias T##_bias");
        m.extend_from_slice(&[ELEM_BOOL, 0]);
        step(&mut m, 0x03);
        step(&mut m, 0x97);
        float(&mut m, 100.0);
        step(&mut m, 0x89);
        store_text(&mut m, "##_ppm");
        for v in [0.5, -5.0, 5.0, 0.5] {
            float(&mut m, v);
        }
        int(&mut m, 3);
        let menu = parse(&m).unwrap();
        assert_eq!(
            menu,
            vec![
                Widget::Combo {
                    id: "##_rate".into(),
                    value: 1,
                    items: vec!["2.5MSPS".into(), "10MSPS".into()],
                },
                Widget::Label("LNA Gain".into()),
                Widget::SliderInt {
                    id: "##_lna".into(),
                    value: 7,
                    min: 0,
                    max: 15,
                },
                Widget::Disabled(true),
                Widget::Checkbox {
                    id: "Bias T##_bias".into(),
                    value: false,
                },
                Widget::Disabled(false),
                Widget::SliderFloat {
                    id: "##_ppm".into(),
                    value: 0.5,
                    min: -5.0,
                    max: 5.0,
                    step: 0.5,
                },
            ]
        );
        assert_eq!(shown("Bias T##_bias"), "Bias T");
        assert_eq!(shown("##_lna"), "");
        // Cut short, it is not a menu.
        assert!(parse(&m[..m.len() - 2]).is_none());
    }

    #[test]
    fn an_action_is_packed() {
        // Send-back flag, the label as a string item, the value as its item.
        assert_eq!(
            action("##g", &Value::Int(9)),
            [1, ELEM_STRING, 3, 0, b'#', b'#', b'g', ELEM_INT, 9, 0, 0, 0]
        );
        assert_eq!(
            action("x", &Value::Bool(true)),
            [1, ELEM_STRING, 1, 0, b'x', ELEM_BOOL, 1]
        );
    }
}
