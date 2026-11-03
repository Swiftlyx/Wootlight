//! Whether a player reports playback in macOS Now Playing (the media controls in Control Center):
//! Music, Spotify, a browser tab and so on.
//!
//! Now Playing lives in the private MediaRemote framework. Since macOS 15.4 its daemon answers
//! only entitled callers, so this process cannot ask it directly. `/usr/bin/perl` is an Apple
//! platform binary that mediaremoted still answers (the trick behind the open-source
//! "mediaremote-adapter"). Rather than loading a helper dylib into perl, as that project does,
//! the script below uses the PerlObjCBridge that ships with the system perl to call
//! `MRNowPlayingRequest` (`localIsPlaying`, `localNowPlayingPlayerPath`) directly, so nothing
//! extra is bundled. Verified on macOS 26.6 with Chrome as the player.
//!
//! One long-lived perl process polls four times a second and prints a line when the state
//! changes; it exits when its stdout closes or this process goes away. Limits: only the app
//! macOS currently shows in Now Playing is seen (one session, not one per app), and should Apple
//! remove the perl bridge or close the loophole `connect` fails, after which the engine listens
//! to the whole device.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use crate::log;

const PERL: &str = "/usr/bin/perl";
/// How long `connect` waits for the helper to report that MediaRemote answers.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Prints `ready`, then `state\t<bundle id>\t<0|1>\t<pid>` on every change (an empty id when
/// nothing is in Now Playing), or `error\t<text>` and exits.
const SCRIPT: &str = r#"
use strict; use warnings;
$| = 1;
$SIG{PIPE} = sub { exit 0 };
eval { require Foundation; Foundation->import; 1 } or do { print "error\tno PerlObjCBridge\n"; exit 1 };
my $bundle = NSBundle->bundleWithPath_("/System/Library/PrivateFrameworks/MediaRemote.framework");
unless ($bundle && $$bundle && $bundle->load) { print "error\tMediaRemote did not load\n"; exit 1 }
my $class = $bundle->classNamed_("MRNowPlayingRequest");
unless ($class && $$class) { print "error\tMRNowPlayingRequest is missing\n"; exit 1 }
@MRNowPlayingRequest::ISA = ("PerlObjCBridge");
sub text { my $s = shift; return "" unless $s && $$s; my $t = $s->UTF8String; $t = "" unless defined $t; $t =~ s/[\t\r\n]//g; return $t }
sub read_state {
    my $playing = MRNowPlayingRequest->localIsPlaying ? 1 : 0;
    my ($id, $pid) = ("", 0);
    my $path = MRNowPlayingRequest->localNowPlayingPlayerPath;
    if ($path && $$path) {
        my $client = $path->client;
        if ($client && $$client) {
            $id = text($client->parentApplicationBundleIdentifier);
            $id = text($client->bundleIdentifier) if $id eq "";
            $pid = $client->processIdentifier || 0;
        }
    }
    $playing = 0 if $id eq "";
    return "$id\t$playing\t$pid";
}
my $parent = getppid();
my $last = "";
my $first = 1;
while (1) {
    exit 0 if getppid() != $parent;
    my $state = eval { read_state() };
    if (!defined $state) {
        my $e = $@; $e =~ s/[\t\r\n]+/ /g;
        print "error\t$e\n"; exit 1;
    }
    if ($first) { print "ready\n"; $first = 0 }
    if ($state ne $last) { print "state\t$state\n"; $last = $state }
    select(undef, undef, undef, 0.25);
}
"#;

#[derive(Default)]
struct State {
    /// Bundle id of the app in Now Playing, its pid and whether it plays.
    current: Option<(String, u32, bool)>,
    /// Why the helper stopped, once it has.
    ended: Option<String>,
}

/// Pids Now Playing last reported per bundle id, for `player::find`.
static PIDS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// The pid Now Playing last reported for `id`.
pub fn reported_pid(id: &str) -> Option<u32> {
    PIDS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(id).copied().filter(|&pid| pid > 0)
}

pub struct MediaSessions {
    child: Child,
    state: Arc<Mutex<State>>,
}

/// Parses `state\t<id>\t<0|1>\t<pid>`.
fn parse_state(line: &str) -> Option<Option<(String, u32, bool)>> {
    let mut parts = line.strip_prefix("state\t")?.split('\t');
    let id = parts.next()?.to_string();
    let playing = parts.next()? == "1";
    let pid = parts.next().and_then(|p| p.trim().parse().ok()).unwrap_or(0);
    Some((!id.is_empty()).then_some((id, pid, playing)))
}

impl MediaSessions {
    /// Starts the Now Playing helper; blocks until it answers, at most a few seconds.
    pub fn connect() -> Result<Self, String> {
        let mut child = Command::new(PERL)
            .args(["-e", SCRIPT])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {PERL}: {e}"))?;
        let stdout = child.stdout.take().ok_or("no pipe to the Now Playing helper")?;
        let state = Arc::new(Mutex::new(State::default()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let shared = Arc::clone(&state);
        let spawned = thread::Builder::new().name("now playing".into()).spawn(move || {
            let mut ready = Some(ready_tx);
            let mut ended = "the Now Playing helper exited".to_string();
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line == "ready" {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Ok(()));
                    }
                } else if let Some(e) = line.strip_prefix("error\t") {
                    ended = format!("Now Playing: {}", e.trim());
                    break;
                } else if let Some(current) = parse_state(&line) {
                    shared.lock().unwrap_or_else(|e| e.into_inner()).current = current;
                }
            }
            if let Some(tx) = ready.take() {
                let _ = tx.send(Err(ended.clone()));
            }
            shared.lock().unwrap_or_else(|e| e.into_inner()).ended = Some(ended);
        });
        let ready = match spawned {
            Ok(_) => ready_rx.recv_timeout(READY_TIMEOUT).unwrap_or_else(|_| Err("Now Playing did not answer".into())),
            Err(e) => Err(format!("cannot start the Now Playing reader: {e}")),
        };
        let sessions = Self { child, state };
        ready?;
        log!("Now Playing connected through {PERL}");
        Ok(sessions)
    }

    /// The app in Now Playing (its bundle id) and whether it reports playback.
    pub fn sessions(&self) -> Result<Vec<(String, bool)>, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = &state.ended {
            return Err(e.clone());
        }
        let Some((id, pid, playing)) = state.current.clone() else { return Ok(Vec::new()) };
        PIDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().insert(id.clone(), pid);
        Ok(vec![(id, playing)])
    }
}

impl Drop for MediaSessions {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_lines_parse() {
        assert_eq!(parse_state("state\tcom.google.Chrome\t1\t697"), Some(Some(("com.google.Chrome".into(), 697, true))));
        assert_eq!(parse_state("state\tcom.apple.Music\t0\t12"), Some(Some(("com.apple.Music".into(), 12, false))));
        assert_eq!(parse_state("state\t\t0\t0"), Some(None));
        assert_eq!(parse_state("ready"), None);
    }

    /// The helper starts and answers on any Mac; with nothing playing the list may be empty.
    #[test]
    #[ignore]
    fn connects() {
        let media = MediaSessions::connect().expect("Now Playing");
        thread::sleep(Duration::from_millis(500));
        println!("{:?}", media.sessions().expect("sessions"));
    }

    /// Start playback (Music, Spotify, a browser tab), then:
    /// `cargo test probe_now_playing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe_now_playing() {
        let media = MediaSessions::connect().expect("Now Playing");
        thread::sleep(Duration::from_millis(500));
        let list = media.sessions().expect("sessions");
        assert!(!list.is_empty(), "nothing in Now Playing");
        for (id, playing) in list {
            let app = crate::player::find(&id);
            println!("media: {id} playing={playing} -> {app:?}");
            assert!(app.is_some(), "no process for {id}");
        }
    }
}
