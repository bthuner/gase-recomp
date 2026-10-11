//! Phone builds: gase on a phone asks the studio on a computer of the same
//! network to turn a game into an app, and installs the result.
//!
//! A phone has no compiler, so the work is done where there is one. The
//! protocol is deliberately small, plain sockets from the standard library:
//!
//! ```text
//!  phone (gase)                                   computer (studio)
//!  ────────────                                   ─────────────────
//!  UDP broadcast  "GASE-RECOMP DISCOVER 1"  ───►  port 38870
//!                 "GASE-RECOMP STUDIO 1 38870 <name>"  ◄───  (to the sender)
//!  POST /builds?title=ALADDIN   (the ROM)   ───►  "Accept?" in the window
//!                 201 "7"                   ◄───  (or 403 once refused)
//!  GET /builds/7, every second              ───►
//!                 "running\n<stage>\n<log>" ◄───  …  "done 41943040"
//!  GET /builds/7/apk                        ───►
//!                 the APK                   ◄───
//! ```
//!
//! Nothing leaves the local network, nothing is typed on the phone, and
//! nothing is built without someone clicking Accept on the computer.
//! [`server::Server`] is the studio's half, [`client`] the phone's.

pub mod client;
mod http;
pub mod server;

/// The UDP port of discovery and the default TCP port of the server.
pub const PORT: u16 = 38870;
/// A ROM bigger than this is refused (the biggest Mega Drive games are 5 MB).
pub const MAX_ROM: usize = 8 << 20;

const DISCOVER: &str = "GASE-RECOMP DISCOVER 1";
const ANNOUNCE: &str = "GASE-RECOMP STUDIO 1";

/// Where a build is, as the phone sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Waiting for someone to click Accept on the computer.
    Waiting,
    Running {
        stage: String,
        /// The last line of the build's log.
        log: String,
    },
    /// The APK is ready (this many bytes).
    Done(usize),
    Failed(String),
    Refused,
}

impl Progress {
    fn encode(&self) -> String {
        match self {
            Self::Waiting => "waiting".into(),
            Self::Running { stage, log } => format!("running\n{stage}\n{log}"),
            Self::Done(n) => format!("done {n}"),
            Self::Failed(e) => format!("failed\n{e}"),
            Self::Refused => "refused".into(),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.splitn(3, '\n');
        let first = lines.next()?;
        Some(match first {
            "waiting" => Self::Waiting,
            "running" => Self::Running {
                stage: lines.next().unwrap_or("").into(),
                log: lines.next().unwrap_or("").into(),
            },
            "refused" => Self::Refused,
            "failed" => Self::Failed(lines.next().unwrap_or("").into()),
            _ => Self::Done(first.strip_prefix("done ")?.parse().ok()?),
        })
    }
}

/// `ALADDIN & co` → `ALADDIN%20%26%20co`.
fn encode_query(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn decode_query(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_round_trips() {
        for p in [
            Progress::Waiting,
            Progress::Running {
                stage: "Build the APK".into(),
                log: "Compiling gase-core".into(),
            },
            Progress::Done(41_943_040),
            Progress::Failed("no Android SDK".into()),
            Progress::Refused,
        ] {
            assert_eq!(Progress::decode(&p.encode()), Some(p));
        }
    }

    #[test]
    fn titles_survive_the_query_string() {
        for t in ["ALADDIN", "SONIC & KNUCKLES", "100% café"] {
            assert_eq!(decode_query(&encode_query(t)), t);
        }
    }
}
