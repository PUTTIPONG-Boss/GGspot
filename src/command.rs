//! The `ggspot:~$` command line: what can be typed and what it means.

pub struct Spec {
    pub name: &'static str,
    pub arg: Option<&'static str>,
    /// Enter on a bare command waits for this argument instead of running.
    pub required: bool,
    pub about: &'static str,
}

impl Spec {
    pub fn usage(&self) -> String {
        match self.arg {
            Some(arg) => format!("/{} {arg}", self.name),
            None => format!("/{}", self.name),
        }
    }
}

const fn plain(name: &'static str, about: &'static str) -> Spec {
    Spec { name, arg: None, required: false, about }
}

const fn with_arg(name: &'static str, arg: &'static str, about: &'static str) -> Spec {
    Spec { name, arg: Some(arg), required: true, about }
}

pub const COMMANDS: &[Spec] = &[
    plain("help", "list every command and key"),
    with_arg("srh", "<song or artist>", "search your library and spotify"),
    Spec { name: "play", arg: Some("[song]"), required: false, about: "play the top match, or resume" },
    plain("pause", "pause"),
    plain("next", "next song"),
    plain("prev", "previous song"),
    plain("shuf", "shuffle on / off"),
    plain("rpt", "repeat: off, all, one"),
    plain("auto", "autoplay on / off"),
    plain("art", "album cover on / off"),
    with_arg("vol", "<0-100>", "set the volume"),
    plain("mini", "spinning record mode"),
    plain("liked", "open liked songs"),
    plain("queue", "open the queue"),
    with_arg("open", "<playlist>", "open a playlist"),
    plain("sort", "sort playlists: recent, custom, a-z"),
    plain("quit", "close ggspot"),
];

#[derive(Debug, PartialEq)]
pub enum Action {
    Help,
    Search(String),
    Play(Option<String>),
    Pause,
    Next,
    Prev,
    Shuffle,
    Repeat,
    Autoplay,
    Art,
    Volume(u8),
    Mini,
    Liked,
    Queue,
    Open(String),
    Sort,
    Quit,
}

fn split(line: &str) -> Option<(String, &str)> {
    let rest = line.trim_start().strip_prefix('/')?;
    let (word, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((word.to_lowercase(), arg.trim()))
}

fn usage(name: &str) -> String {
    let usage = COMMANDS.iter().find(|c| c.name == name).map_or_else(String::new, Spec::usage);
    format!("usage: {usage}")
}

pub fn parse(line: &str) -> Result<Action, String> {
    let line = line.trim();
    let Some((word, arg)) = split(line) else {
        return Err(format!("not a command: {line}  (try /help)"));
    };
    let text = |name: &str| if arg.is_empty() { Err(usage(name)) } else { Ok(arg.to_string()) };
    Ok(match word.as_str() {
        "help" => Action::Help,
        "srh" => Action::Search(text("srh")?),
        "play" => Action::Play((!arg.is_empty()).then(|| arg.to_string())),
        "pause" => Action::Pause,
        "next" => Action::Next,
        "prev" => Action::Prev,
        "shuf" => Action::Shuffle,
        "rpt" => Action::Repeat,
        "auto" => Action::Autoplay,
        "art" => Action::Art,
        "vol" => Action::Volume(arg.parse().ok().filter(|v| *v <= 100).ok_or_else(|| usage("vol"))?),
        "mini" => Action::Mini,
        "liked" => Action::Liked,
        "queue" => Action::Queue,
        "open" => Action::Open(text("open")?),
        "sort" => Action::Sort,
        "quit" => Action::Quit,
        _ => return Err(format!("unknown command: /{word}  (try /help)")),
    })
}

/// What the results area should show while this line is being typed.
pub fn live_query(line: &str) -> Option<&str> {
    let (word, arg) = split(line)?;
    (matches!(word.as_str(), "srh" | "play") && !arg.is_empty()).then_some(arg)
}

pub struct Suggestion {
    /// The line after accepting this; a trailing space means it wants more typing.
    pub fill: String,
    pub label: String,
    pub about: String,
    /// Enter fills it in rather than running it.
    pub wants_more: bool,
}

/// What could come next on `line`: commands, then playlist names for `/open`.
pub fn suggest(line: &str, playlists: &[&str]) -> Vec<Suggestion> {
    let Some(rest) = line.strip_prefix('/') else { return Vec::new() };
    let Some((word, arg)) = rest.split_once(' ') else {
        let word = rest.to_lowercase();
        return COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(&word))
            .map(|c| Suggestion {
                fill: format!("/{}{}", c.name, if c.arg.is_some() { " " } else { "" }),
                label: c.usage(),
                about: c.about.into(),
                wants_more: c.required,
            })
            .collect();
    };
    if !word.eq_ignore_ascii_case("open") {
        return Vec::new();
    }
    let arg = arg.trim().to_lowercase();
    playlists
        .iter()
        .filter(|p| p.to_lowercase().contains(&arg))
        .map(|p| Suggestion { fill: format!("/open {p}"), label: p.to_string(), about: "playlist".into(), wants_more: false })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_command_parses() {
        for c in COMMANDS {
            let line = match c.arg {
                Some("<0-100>") => format!("/{} 40", c.name),
                Some(_) => format!("/{} x", c.name),
                None => format!("/{}", c.name),
            };
            assert!(parse(&line).is_ok(), "{line}");
        }
    }

    #[test]
    fn arguments_keep_their_words_and_commands_ignore_case() {
        assert_eq!(parse("  /PLAY   King Gnu  "), Ok(Action::Play(Some("King Gnu".into()))));
        assert_eq!(parse("/play"), Ok(Action::Play(None)));
        assert_eq!(parse("/srh 飛行艇"), Ok(Action::Search("飛行艇".into())));
        assert_eq!(parse("/vol 100"), Ok(Action::Volume(100)));
    }

    #[test]
    fn mistakes_explain_themselves() {
        assert_eq!(parse("/srh"), Err("usage: /srh <song or artist>".into()));
        assert_eq!(parse("/vol 101"), Err("usage: /vol <0-100>".into()));
        assert_eq!(parse("/vol loud"), Err("usage: /vol <0-100>".into()));
        assert!(parse("/nope").unwrap_err().contains("/help"));
        assert!(parse("hello").unwrap_err().contains("/help"));
        assert!(parse("/").is_err());
    }

    #[test]
    fn search_and_play_show_results_while_typing() {
        assert_eq!(live_query("/srh king gnu"), Some("king gnu"));
        assert_eq!(live_query("/play aizo"), Some("aizo"));
        assert_eq!(live_query("/srh "), None);
        assert_eq!(live_query("/mini"), None);
        assert_eq!(live_query("aizo"), None);
    }

    fn fills(line: &str) -> Vec<String> {
        suggest(line, &["Bruno Mars Radio", "JP Vibe"]).into_iter().map(|s| s.fill).collect()
    }

    #[test]
    fn slash_lists_everything_and_typing_narrows_it() {
        assert_eq!(fills("/").len(), COMMANDS.len());
        assert_eq!(fills("/mi"), ["/mini"]);
        assert_eq!(fills("/P"), ["/play ", "/pause", "/prev"]);
        assert!(fills("/zzz").is_empty());
        assert!(fills("plain").is_empty());
        assert!(fills("/play song").is_empty());
    }

    #[test]
    fn open_suggests_playlists() {
        assert_eq!(fills("/open vib"), ["/open JP Vibe"]);
        assert_eq!(fills("/open ").len(), 2);
    }

    #[test]
    fn enter_waits_for_a_required_argument_only() {
        let wants = |l: &str| suggest(l, &[])[0].wants_more;
        assert!(wants("/sr"));
        assert!(!wants("/pl"));
        assert!(!wants("/mi"));
    }
}
