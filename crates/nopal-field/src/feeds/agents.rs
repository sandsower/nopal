//! Process-tree agent-presence poller.
//!
//! The seat glyph and the `s` key both need to know whether a pane is
//! currently running the agent. The obvious signal, tmux's
//! `pane_current_command`, is what the pane's own subscription already
//! carries as [`crate::state::Seat::command`] - but it only reports the
//! foreground process attached to the pane's pty, and that is not always
//! the agent even while it runs. Shell-integration wrappers such as
//! kiro-cli-term execute the operator's commands on a separate pty and
//! leave the visible pane's foreground command reporting the login shell
//! (`zsh`) forever, so `pane_current_command == "nopal"` never fires there
//! even mid-run. The launched agent is still a descendant of the pane's
//! shell process in that shape (`pane_pid -> /bin/zsh --login -> nopal
//! cli`, two levels down) - process-tree walking finds it where the
//! foreground-command check cannot.
//!
//! This feed polls `tmux list-panes -a` and `ps -axo pid=,ppid=,command=`
//! every tick, walks each pane's process subtree for the agent binary, and
//! reports the full set of matching panes as one [`FeedEvent::AgentPanes`]
//! snapshot. It never trusts `pane_current_command`; [`crate::state::App`]
//! keeps that check too, as a fallback for panes running the agent
//! directly with no shell wrapper in the way.
//!
//! "The agent binary" is two needles, not one: the nopal launcher only
//! exists until `nopal cli` execs into pi, and pi (a node script) rewrites
//! its ps title to plain `pi` - so a live seat's subtree contains neither
//! the nopal path nor a `node .../pi` argv, as observed in an end-to-end run.
//! Detection therefore matches a process whose command's first token is,
//! or has the basename of, either the nopal binary or the pi binary
//! (`NOPAL_PI_BIN` or `pi`, mirroring the launcher's exec convention).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::feeds::Feed;
use crate::state::FeedEvent;

pub const SOURCE: &str = "agents";
const POLL_INTERVAL: Duration = Duration::from_millis(2500);

/// Polls tmux and `ps` for process-tree agent presence; see the module doc
/// for why `pane_current_command` alone cannot answer this question.
pub struct AgentPanesFeed {
    /// The agent needles: the nopal binary path (pre-exec launcher, test
    /// stubs) and the pi binary (`NOPAL_PI_BIN` or `pi`) the launcher execs
    /// into; see [`matches_agent`] for how a process command is compared.
    needles: Vec<String>,
}

impl AgentPanesFeed {
    pub fn new(nopal_bin: PathBuf, pi_bin: String) -> Self {
        Self {
            needles: vec![nopal_bin.to_string_lossy().into_owned(), pi_bin],
        }
    }
}

impl Feed for AgentPanesFeed {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    fn poll(&mut self) -> Result<Vec<FeedEvent>, String> {
        let panes = list_panes()?;
        let procs = list_procs()?;
        Ok(vec![FeedEvent::AgentPanes(agent_panes(
            &panes,
            &procs,
            &self.needles,
        ))])
    }
}

/// A pane is agent-running when the process at `pane_pid`, or any
/// descendant of it in the `ppid` tree, matches one of `needles` (see
/// [`matches_agent`]). Descent has no depth limit and is cycle-safe:
/// malformed `ps` snapshots (a pid briefly reparented to a running child,
/// a stale duplicate line) cannot walk the same pid twice.
pub fn agent_panes(
    panes: &[(String, u32)],
    procs: &[(u32, u32, String)],
    needles: &[String],
) -> BTreeSet<String> {
    let mut command_by_pid: HashMap<u32, &str> = HashMap::new();
    let mut children_by_ppid: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, ppid, command) in procs {
        command_by_pid.insert(*pid, command.as_str());
        children_by_ppid.entry(*ppid).or_default().push(*pid);
    }

    panes
        .iter()
        .filter(|(_, pane_pid)| {
            subtree_runs_agent(*pane_pid, &command_by_pid, &children_by_ppid, needles)
        })
        .map(|(pane_id, _)| pane_id.clone())
        .collect()
}

/// A process command is the agent when its first whitespace token equals a
/// needle, or the token's basename equals the needle's basename. Token
/// equality (not `contains`) keeps `pip`/`nopaly`-style near-names out;
/// the basename comparison is what lets the needle `pi` match the ps title
/// `pi` that node writes over pi's real `node /path/to/pi` argv, and lets
/// a path-valued `NOPAL_PI_BIN` match a title that dropped the directory.
fn matches_agent(command: &str, needles: &[String]) -> bool {
    let Some(token) = command.split_whitespace().next() else {
        return false;
    };
    needles
        .iter()
        .any(|needle| token == needle || basename(token) == basename(needle))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Depth-first walk from `root` over the `ppid` tree, guarded against
/// cycles by a visited set (a self-loop or a mutual pair in the snapshot
/// stops immediately instead of spinning).
fn subtree_runs_agent(
    root: u32,
    command_by_pid: &HashMap<u32, &str>,
    children_by_ppid: &HashMap<u32, Vec<u32>>,
    needles: &[String],
) -> bool {
    let mut stack = vec![root];
    let mut visited = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if command_by_pid
            .get(&pid)
            .is_some_and(|command| matches_agent(command, needles))
        {
            return true;
        }
        if let Some(children) = children_by_ppid.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }
    false
}

/// `tmux list-panes -a -F "#{pane_id}|#{pane_pid}"`, server-wide (every
/// session, not just the field's own) so foreign-session seats get the
/// same detection.
fn list_panes() -> Result<Vec<(String, u32)>, String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}|#{pane_pid}"])
        .output()
        .map_err(|err| format!("tmux list-panes failed: {err}"))?;
    if !output.status.success() {
        return Err(format!("tmux list-panes exited {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_pane_line).collect())
}

fn parse_pane_line(line: &str) -> Option<(String, u32)> {
    let (pane_id, pid) = line.split_once('|')?;
    let pid: u32 = pid.trim().parse().ok()?;
    Some((pane_id.to_owned(), pid))
}

/// `ps -axo pid=,ppid=,command=`: every process on the machine, no header
/// row (the trailing `=` on each field suppresses it). Columns are
/// whitespace-padded for alignment, so parsing takes the first two
/// whitespace-delimited tokens and treats the remainder (trimmed) as the
/// command - a plain split on whitespace would fragment commands that
/// contain spaces in their arguments.
fn list_procs() -> Result<Vec<(u32, u32, String)>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .map_err(|err| format!("ps failed: {err}"))?;
    if !output.status.success() {
        return Err(format!("ps exited {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_ps_line).collect())
}

fn parse_ps_line(line: &str) -> Option<(u32, u32, String)> {
    let mut rest = line;
    let pid: u32 = take_token(&mut rest)?.parse().ok()?;
    let ppid: u32 = take_token(&mut rest)?.parse().ok()?;
    let command = rest.trim().to_owned();
    if command.is_empty() {
        return None;
    }
    Some((pid, ppid, command))
}

/// Pop the next whitespace-delimited token off the front of `rest`,
/// tolerating any run length of leading/inner whitespace.
fn take_token<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let trimmed = rest.trim_start();
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    if end == 0 {
        return None;
    }
    let (token, remainder) = trimmed.split_at(end);
    *rest = remainder;
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOPAL: &str = "/usr/local/bin/nopal";

    fn needles() -> Vec<String> {
        vec![NOPAL.to_owned(), "pi".to_owned()]
    }

    #[test]
    fn direct_match_on_pane_pid() {
        let panes = vec![("%1".to_owned(), 100)];
        let procs = vec![(100, 1, format!("{NOPAL} cli"))];
        assert_eq!(
            agent_panes(&panes, &procs, &needles()),
            BTreeSet::from(["%1".to_owned()])
        );
    }

    /// The kiro-cli-term shape: the pane's own process is a shell wrapper,
    /// its child is the login shell, and the agent is that shell's child -
    /// two levels below `pane_pid`.
    #[test]
    fn two_level_descendant_match() {
        let panes = vec![("%2".to_owned(), 200)];
        let procs = vec![
            (200, 1, "zsh".to_owned()),
            (201, 200, "/bin/zsh --login".to_owned()),
            (202, 201, format!("{NOPAL} cli")),
        ];
        assert_eq!(
            agent_panes(&panes, &procs, &needles()),
            BTreeSet::from(["%2".to_owned()])
        );
    }

    /// The post-exec shape observed in an end-to-end run: `nopal cli` execs
    /// into pi, and pi's node runtime rewrites the ps title to plain `pi` -
    /// no nopal path anywhere in the subtree.
    #[test]
    fn pi_title_after_exec_matches() {
        let panes = vec![("%2".to_owned(), 200)];
        let procs = vec![
            (200, 1, "zsh (kiro-cli-term)".to_owned()),
            (201, 200, "/bin/zsh --login".to_owned()),
            (202, 201, "pi ".to_owned()),
        ];
        assert_eq!(
            agent_panes(&panes, &procs, &needles()),
            BTreeSet::from(["%2".to_owned()])
        );
    }

    /// A path-valued first token still matches the bare `pi` needle by
    /// basename (a native pi build, or a title that kept the path).
    #[test]
    fn pi_path_matches_bare_needle_by_basename() {
        assert!(matches_agent("/opt/homebrew/bin/pi --resume", &needles()));
    }

    /// Token equality, not substring: near-names and mid-argv mentions of
    /// a needle are not the agent.
    #[test]
    fn near_names_and_argv_mentions_do_not_match() {
        assert!(!matches_agent(
            "/opt/homebrew/bin/pip install pi",
            &needles()
        ));
        assert!(!matches_agent("nopaly --serve", &needles()));
        assert!(!matches_agent(format!("vim {NOPAL}").as_str(), &needles()));
        assert!(!matches_agent("", &needles()));
    }

    #[test]
    fn no_match_when_needle_absent() {
        let panes = vec![("%3".to_owned(), 300)];
        let procs = vec![(300, 1, "zsh".to_owned()), (301, 300, "vim".to_owned())];
        assert!(agent_panes(&panes, &procs, &needles()).is_empty());
    }

    #[test]
    fn cycle_in_ppid_data_does_not_hang() {
        let panes = vec![("%4".to_owned(), 400)];
        // 400 -> 401 -> 400: a mutual cycle in the snapshot.
        let procs = vec![(400, 401, "zsh".to_owned()), (401, 400, "zsh".to_owned())];
        assert!(agent_panes(&panes, &procs, &needles()).is_empty());

        // The same cycle still finds a match reachable from the root.
        let procs_with_agent = vec![
            (400, 401, "zsh".to_owned()),
            (401, 400, "zsh".to_owned()),
            (402, 400, format!("{NOPAL} cli")),
        ];
        assert_eq!(
            agent_panes(&panes, &procs_with_agent, &needles()),
            BTreeSet::from(["%4".to_owned()])
        );
    }

    #[test]
    fn multiple_panes_each_evaluated_independently() {
        let panes = vec![
            ("%5".to_owned(), 500),
            ("%6".to_owned(), 600),
            ("%7".to_owned(), 700),
        ];
        let procs = vec![
            (500, 1, "zsh".to_owned()),
            (501, 500, format!("{NOPAL} cli")),
            (600, 1, "zsh".to_owned()),
            (601, 600, "vim".to_owned()),
            (700, 1, format!("{NOPAL} cli")),
        ];
        assert_eq!(
            agent_panes(&panes, &procs, &needles()),
            BTreeSet::from(["%5".to_owned(), "%7".to_owned()])
        );
    }

    #[test]
    fn malformed_pane_and_ps_lines_are_tolerated() {
        assert_eq!(parse_pane_line("no-pipe-here"), None);
        assert_eq!(parse_pane_line("%1|not-a-number"), None);
        assert_eq!(parse_pane_line("%1|100"), Some(("%1".to_owned(), 100)));
        assert_eq!(parse_ps_line(""), None);
        assert_eq!(parse_ps_line("not-a-pid ppid command"), None);
        assert_eq!(
            parse_ps_line("  501     1 /usr/libexec/thing -x -y"),
            Some((501, 1, "/usr/libexec/thing -x -y".to_owned()))
        );
    }
}
