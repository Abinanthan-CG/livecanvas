//! Command-line parsing and the single-instance wire protocol.
//!
//! A second `livecanvas.exe` invocation parses its arguments into a
//! [`Command`], serialises it with [`Command::to_wire`] and hands it to the
//! running instance through `WM_COPYDATA`, which decodes it with
//! [`Command::from_wire`].

/// Everything LiveCanvas can be asked to do, from the CLI or from another instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start normally; if a source is given, switch to it.
    Run { source: Option<String> },
    /// Reload the current page.
    Reload,
    /// Shut the running instance down.
    Quit,
    /// Flip mouse forwarding on or off.
    ToggleInteractive,
    /// Mute or unmute page audio.
    SetMuted(bool),
    /// Enable or disable "start with Windows".
    Autostart(bool),
    /// Print usage.
    Help,
    /// Print the version.
    Version,
}

/// Usage text shown for `--help` and after a parse error.
pub const USAGE: &str = "\
LiveCanvas - interactive HTML live wallpaper

USAGE:
    livecanvas.exe [SOURCE]
    livecanvas.exe <COMMAND>

SOURCE:
    A URL (https://...), an .html file, or a folder containing index.html.
    If LiveCanvas is already running, the running instance switches to it.

COMMANDS:
    --set <SOURCE>          Same as passing SOURCE directly
    --reload                Reload the current page
    --toggle-interactive    Toggle mouse forwarding
    --mute | --unmute       Control page audio
    --autostart <on|off>    Start with Windows (per-user, no admin rights)
    --quit                  Stop the running instance
    --help | --version";

impl Command {
    /// Parses process arguments (without the executable name).
    pub fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = args.into_iter().map(Into::into);
        let Some(first) = args.next() else {
            return Ok(Self::Run { source: None });
        };
        let cmd = match first.as_str() {
            "--set" | "-s" => {
                let source = args.next().ok_or("--set requires a SOURCE argument")?;
                Self::Run { source: Some(source) }
            }
            "--reload" => Self::Reload,
            "--quit" | "--exit" => Self::Quit,
            "--toggle-interactive" => Self::ToggleInteractive,
            "--mute" => Self::SetMuted(true),
            "--unmute" => Self::SetMuted(false),
            "--autostart" => match args.next().as_deref() {
                Some("on") => Self::Autostart(true),
                Some("off") => Self::Autostart(false),
                _ => return Err("--autostart requires `on` or `off`".into()),
            },
            "--help" | "-h" | "/?" => Self::Help,
            "--version" | "-V" => Self::Version,
            flag if flag.starts_with("--") => return Err(format!("unknown option `{flag}`")),
            _ => Self::Run { source: Some(first) },
        };
        match args.next() {
            Some(extra) => Err(format!("unexpected argument `{extra}`")),
            None => Ok(cmd),
        }
    }

    /// Encodes the command for transport to the running instance. Commands
    /// that are handled locally (`Help`, `Version`, `Autostart`) return `None`.
    pub fn to_wire(&self) -> Option<String> {
        Some(match self {
            Self::Run { source: Some(s) } => format!("set\t{s}"),
            Self::Run { source: None } => "show".into(),
            Self::Reload => "reload".into(),
            Self::Quit => "quit".into(),
            Self::ToggleInteractive => "toggle-interactive".into(),
            Self::SetMuted(true) => "mute".into(),
            Self::SetMuted(false) => "unmute".into(),
            Self::Autostart(_) | Self::Help | Self::Version => return None,
        })
    }

    /// Decodes a command received from another instance. Returns `None` for
    /// anything unrecognised: input from other processes is never trusted.
    pub fn from_wire(wire: &str) -> Option<Self> {
        let (verb, arg) = match wire.split_once('\t') {
            Some((verb, arg)) => (verb, Some(arg)),
            None => (wire, None),
        };
        Some(match (verb, arg) {
            ("set", Some(s)) if !s.is_empty() => Self::Run { source: Some(s.to_owned()) },
            ("show", None) => Self::Run { source: None },
            ("reload", None) => Self::Reload,
            ("quit", None) => Self::Quit,
            ("toggle-interactive", None) => Self::ToggleInteractive,
            ("mute", None) => Self::SetMuted(true),
            ("unmute", None) => Self::SetMuted(false),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        Command::parse(args.iter().copied())
    }

    #[test]
    fn no_arguments_runs() {
        assert_eq!(parse(&[]), Ok(Command::Run { source: None }));
    }

    #[test]
    fn bare_source_and_set_are_equivalent() {
        let expected = Ok(Command::Run { source: Some("https://example.com".into()) });
        assert_eq!(parse(&["https://example.com"]), expected);
        assert_eq!(parse(&["--set", "https://example.com"]), expected);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&["--set"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--autostart", "maybe"]).is_err());
        assert!(parse(&["--reload", "extra"]).is_err());
    }

    #[test]
    fn wire_round_trip() {
        let commands = [
            Command::Run { source: Some("C:\\my wall\\index.html".into()) },
            Command::Run { source: None },
            Command::Reload,
            Command::Quit,
            Command::ToggleInteractive,
            Command::SetMuted(true),
            Command::SetMuted(false),
        ];
        for cmd in commands {
            let wire = cmd.to_wire().expect("transportable command");
            assert_eq!(Command::from_wire(&wire), Some(cmd));
        }
    }

    #[test]
    fn local_only_commands_have_no_wire_form() {
        assert_eq!(Command::Help.to_wire(), None);
        assert_eq!(Command::Autostart(true).to_wire(), None);
    }

    #[test]
    fn untrusted_wire_input_is_rejected() {
        assert_eq!(Command::from_wire(""), None);
        assert_eq!(Command::from_wire("set\t"), None);
        assert_eq!(Command::from_wire("format-disk"), None);
        assert_eq!(Command::from_wire("quit\tnow"), None);
    }
}
