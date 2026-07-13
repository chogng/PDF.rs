use std::env;
use std::process::ExitCode;

struct Selection {
    lane: &'static str,
    reason: &'static str,
    checks: &'static str,
}

fn selection_for(lane: &str) -> Option<Selection> {
    match lane {
        "local" => Some(Selection {
            lane: "local",
            reason: "pre-submit feedback for formatting, lint, and workspace tests",
            checks: "fmt,clippy,test",
        }),
        "pr" => Some(Selection {
            lane: "pr",
            reason: "merge gate for the complete required Rust quality baseline",
            checks: "fmt,clippy,test,doc",
        }),
        _ => None,
    }
}

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let lane = arguments.next();

    if arguments.next().is_some() {
        eprintln!("usage: pdf-rs-quality <local|pr>");
        return ExitCode::from(2);
    }

    let Some(selection) = lane.as_deref().and_then(selection_for) else {
        eprintln!("usage: pdf-rs-quality <local|pr>");
        return ExitCode::from(2);
    };

    println!("lane={}", selection.lane);
    println!("selection_reason={}", selection.reason);
    println!("checks={}", selection.checks);

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::selection_for;

    #[test]
    fn recognizes_supported_lanes() {
        assert_eq!(
            selection_for("local").map(|selection| selection.lane),
            Some("local")
        );
        assert_eq!(
            selection_for("pr").map(|selection| selection.lane),
            Some("pr")
        );
    }

    #[test]
    fn rejects_unknown_lanes() {
        assert!(selection_for("nightly").is_none());
    }
}
