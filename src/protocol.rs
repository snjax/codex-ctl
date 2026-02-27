use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Spawn {
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        gui: bool,
        #[serde(default)]
        resume: Option<String>,
    },
    List,
    State {
        session: String,
        #[serde(default)]
        wait: Option<Vec<String>>,
        #[serde(default)]
        timeout: Option<f64>,
    },
    Log {
        session: String,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        follow: bool,
        #[serde(default)]
        since: Option<u64>,
        #[serde(default)]
        wait: bool,
        #[serde(default)]
        timeout: Option<f64>,
    },
    Last {
        session: String,
    },
    Act {
        session: String,
        actions: Vec<String>,
    },
    Screen {
        session: String,
        #[serde(default)]
        clean: bool,
        #[serde(default)]
        raw: bool,
    },
    Expand {
        session: String,
        block_ids: Vec<String>,
    },
    Gui {
        session: String,
    },
    Kill {
        session: String,
    },
    GuiAttach {
        session: String,
    },
    Ping,
}

pub fn ok_json(data: serde_json::Value) -> serde_json::Value {
    data
}

pub fn err_json(msg: &str) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": msg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serde_roundtrip_spawn() {
        let json = r#"{"cmd":"spawn","prompt":"hello","cwd":"/tmp","gui":false}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::Spawn { prompt, cwd, gui, resume } => {
                assert_eq!(prompt.as_deref(), Some("hello"));
                assert_eq!(cwd.as_deref(), Some("/tmp"));
                assert!(!gui);
                assert!(resume.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_serde_roundtrip_spawn_with_resume() {
        let json = r#"{"cmd":"spawn","resume":"019c8826-8134-7183-be06-6f93dd6dd5e5","prompt":"continue","cwd":"/tmp"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::Spawn { prompt, cwd, resume, .. } => {
                assert_eq!(prompt.as_deref(), Some("continue"));
                assert_eq!(cwd.as_deref(), Some("/tmp"));
                assert_eq!(resume.as_deref(), Some("019c8826-8134-7183-be06-6f93dd6dd5e5"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_serde_roundtrip_spawn_resume_no_prompt() {
        let json = r#"{"cmd":"spawn","resume":"019c8826-8134-7183-be06-6f93dd6dd5e5"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::Spawn { prompt, resume, .. } => {
                assert!(prompt.is_none());
                assert_eq!(resume.as_deref(), Some("019c8826-8134-7183-be06-6f93dd6dd5e5"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_request_serde_roundtrip_list() {
        let json = r#"{"cmd":"list"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert!(matches!(req, Request::List));
    }

    #[test]
    fn test_request_serde_roundtrip_ping() {
        let json = r#"{"cmd":"ping"}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert!(matches!(req, Request::Ping));
    }

    #[test]
    fn test_err_json() {
        let resp = err_json("something failed");
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"], "something failed");
    }
}
