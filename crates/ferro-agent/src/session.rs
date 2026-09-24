//! Session log: every ask + applied patch appended as Markdown under
//! `{root}/.ferro/sessions/<id>.md`. The training signal and the audit trail.

use crate::{ApplyReport, Transcript};

pub fn session_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".ferro").join("sessions")
}

pub fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", nanos, std::process::id())
}

pub fn log_ask(
    root: &std::path::Path,
    id: &str,
    question: &str,
    transcript: &Transcript,
    applied: &[ApplyReport],
) -> Result<std::path::PathBuf, String> {
    let dir = session_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{id}.md"));
    let mut md = String::from("# Ferro session\n\n");
    md.push_str(&format!("- id: {id}\n- root: {}\n\n", root.display()));
    md.push_str(&format!("## Q\n\n{question}\n\n"));
    for (i, step) in transcript.steps.iter().enumerate() {
        md.push_str(&format!("## Step {}\n\n", i + 1));
        if let Some(t) = &step.thought {
            md.push_str(t);
            md.push_str("\n\n");
        }
        for (call, result) in &step.calls {
            md.push_str(&format!(
                "### $ {} {}\n\n```\n{}\n```\n\n",
                call.name,
                call.args,
                trunc(&result.output)
            ));
        }
    }
    md.push_str(&format!("## Answer\n\n{}\n\n", transcript.final_text));
    for rep in applied {
        md.push_str("## Applied patch\n\n");
        md.push_str(&format!("- files: {}\n", rep.files.join(", ")));
        md.push_str(&format!("- created: {}\n", rep.created.join(", ")));
        md.push_str(&format!("```\n{}\n```\n\n", trunc(&rep.status_after)));
    }
    std::fs::write(&path, md).map_err(|e| e.to_string())?;
    Ok(path)
}

fn trunc(s: &str) -> String {
    const CAP: usize = 8 * 1024;
    if s.len() > CAP {
        format!("{}… (truncated)", ferro_core::text::truncate_utf8(s, CAP))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_markdown_log() {
        let dir = tempfile::tempdir().unwrap();
        let t = Transcript {
            steps: vec![],
            final_text: "hello".into(),
            truncated: false,
        };
        let p = log_ask(dir.path(), "test-1", "hi?", &t, &[]).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("## Q"));
        assert!(body.contains("hello"));
        assert!(p.to_string_lossy().contains(".ferro/sessions/test-1.md"));
    }
}
