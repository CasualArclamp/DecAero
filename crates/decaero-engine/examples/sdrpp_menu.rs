// made by CasualArclamp
//! Print the controls an SDR++ server hands out for its radio, and
//! optionally change one:
//! `sdrpp_menu sdrpp://HOST:PORT [--set ID VALUE]`
//! VALUE is `true`/`false`, a whole number, a number with a point, or
//! `press` for a button. The server's tuning is left alone.

use decaero_engine::Source;
use decaero_engine::net::{self, SdrppOptions};
use decaero_engine::smgui::{self, Value, Widget};
use std::time::{Duration, Instant};

/// The next copy of the menu, waiting up to five seconds for it.
fn menu(source: &mut dyn Source, seen: &mut u64) -> Option<Vec<Widget>> {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(5) {
        // Samples are read and dropped so the reader is never held up.
        source.read(1024)?;
        if let Some((seq, widgets)) = source.menu(*seen) {
            *seen = seq;
            return Some(widgets);
        }
    }
    None
}

fn print(widgets: &[Widget]) {
    for w in widgets {
        match w {
            Widget::Label(text) => println!("  [{text}]"),
            Widget::Text(text) => println!("  text: {text}"),
            Widget::Disabled(on) => {
                println!("  -- {} --", if *on { "disabled" } else { "enabled" })
            }
            Widget::Combo { id, value, items } => println!(
                "  combo {id:?} = {value} ({}) of {items:?}",
                items.get(*value as usize).map_or("?", String::as_str)
            ),
            Widget::Button { id } => println!("  button {id:?} ({})", smgui::shown(id)),
            Widget::Radio { id, on } => println!("  radio {id:?} = {on}"),
            Widget::SliderInt {
                id,
                value,
                min,
                max,
            } => {
                println!("  slider {id:?} = {value} ({min}..{max})");
            }
            Widget::SliderFloat {
                id,
                value,
                min,
                max,
                step,
            } => {
                println!("  slider {id:?} = {value} ({min}..{max}, step {step})");
            }
            Widget::InputInt { id, value } => println!("  number {id:?} = {value}"),
            Widget::Checkbox { id, value } => println!("  check {id:?} = {value}"),
            Widget::InputText { id, value } => println!("  text box {id:?} = {value:?}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(url) = args.first() else {
        eprintln!("usage: sdrpp_menu sdrpp://HOST:PORT [--set ID VALUE]");
        return;
    };
    let mut source =
        match net::open_sdrpp(url.trim_start_matches("sdrpp://"), SdrppOptions::default()) {
            Ok(s) => s,
            Err(e) => return eprintln!("{e}"),
        };
    let mut seen = 0;
    let Some(mut widgets) = menu(&mut source, &mut seen) else {
        return eprintln!("no menu came");
    };
    // The second copy (asked for once the radio runs) is the one to show.
    if let Some(again) = menu(&mut source, &mut seen) {
        widgets = again;
    }
    print(&widgets);
    if let [_, flag, id, value] = args.as_slice()
        && flag == "--set"
    {
        let value = match value.as_str() {
            "press" => Value::Pressed,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            v if v.contains('.') => Value::Float(v.parse().unwrap_or(0.0)),
            v => Value::Int(v.parse().unwrap_or(0)),
        };
        println!("set {id:?} to {value:?}");
        source.menu_action(id, &value);
        match menu(&mut source, &mut seen) {
            Some(after) => print(&after),
            None => eprintln!("no answer to the change"),
        }
    }
}
