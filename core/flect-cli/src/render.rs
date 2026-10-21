//! Turning engine events and the menu export into terminal text.
//!
//! The core has no key -> text table (localization lives in the Swift app's String Catalog), so
//! messages are printed as the plain key followed by its arguments, in English.
use serde_json::Value;

use flect_core::{batch, tasks};

pub fn message(v: &Value) -> String {
    let key = v["key"].as_str().unwrap_or("?");
    let args: Vec<&str> = v["args"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    plain(key, &args)
}

pub fn plain(key: &str, args: &[&str]) -> String {
    if args.is_empty() {
        key.to_owned()
    } else {
        format!("{key}: {}", args.join(", "))
    }
}

/// What a human-readable run printed, so the caller can decide the final line.
#[derive(Default)]
pub struct Tally {
    pub errors: u32,
    pub done: u64,
    pub total: u64,
}

/// One entry of `flect list`.
#[derive(Debug, PartialEq)]
pub struct TaskInfo {
    pub id: String,
    pub label: String,
    /// Values accepted by the `to` option, when the task is a format conversion.
    pub to: Vec<String>,
}

/// Runnable task ids from the menu export: presets/custom/format leaves fold into their task.
pub fn task_list(menu: &Value) -> Vec<TaskInfo> {
    let mut out = vec![];
    let empty = vec![];
    for category in menu["entries"].as_array().unwrap_or(&empty) {
        let cat_label = category["label"].as_str().unwrap_or("");
        let mut direct_to: Vec<(String, String)> = vec![]; // (task id, format)
        for entry in category["children"].as_array().unwrap_or(&empty) {
            let id = entry["id"].as_str().unwrap_or("");
            let kids = entry["children"].as_array().unwrap_or(&empty);
            if entry["label_key"] == "menu.to" {
                if let Some((task, fmt)) = id.rsplit_once('.') {
                    direct_to.push((task.to_owned(), fmt.to_owned()));
                }
                continue;
            }
            let to = kids
                .iter()
                .filter(|k| k["label_key"] == "menu.to")
                .filter_map(|k| {
                    k["id"]
                        .as_str()?
                        .rsplit_once('.')
                        .map(|(_, f)| f.to_owned())
                })
                .collect();
            out.push(TaskInfo {
                id: id.to_owned(),
                label: format!("{cat_label} / {}", entry["label"].as_str().unwrap_or("")),
                to,
            });
        }
        for (task, fmt) in direct_to {
            match out.iter_mut().find(|t| t.id == task) {
                Some(t) => t.to.push(fmt),
                None => out.push(TaskInfo {
                    label: format!("{cat_label} / {}", task.rsplit('.').next().unwrap_or("")),
                    id: task,
                    to: vec![fmt],
                }),
            }
        }
    }
    // Only what the engine can actually run (the catalog also lists app-side tasks).
    out.retain(|t| tasks::is_file_task(&t.id) || batch::is_batch_task(&t.id));
    out
}

pub fn task_line(t: &TaskInfo) -> String {
    let opts = if t.to.is_empty() {
        String::new()
    } else {
        format!("to={}", t.to.join("|"))
    };
    format!("{}\t{}\t{}", t.id, t.label, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_are_key_plus_args() {
        assert_eq!(message(&json!({"key":"msg.x","args":[]})), "msg.x");
        assert_eq!(
            message(&json!({"key":"msg.processing","args":["a.png","b"]})),
            "msg.processing: a.png, b"
        );
    }

    #[test]
    fn list_folds_presets_and_formats() {
        let tasks = task_list(&flect_core::menu::export());
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"folder.tree"));
        assert!(ids.contains(&"text.encode"));
        assert!(ids.contains(&"image.resize"));
        assert!(!ids.contains(&"image.resize.p1"));
        assert!(!ids.contains(&"text.encode.UTF8"));
        let convert = tasks.iter().find(|t| t.id == "text.encode").unwrap();
        assert!(convert.to.contains(&"SJIS".to_owned()));
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
