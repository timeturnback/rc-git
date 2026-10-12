//! The send-pack command line, the `report-status` reply and the server's sideband
//! messages, as pure functions over bytes. gix has no push protocol, so this is written
//! against git's documented `pack-protocol` format and tested with hand-built replies.

use std::collections::VecDeque;

use gix::ObjectId;

/// The capability that makes `git-receive-pack` multiplex its reply: the report on band 1,
/// hook and server messages on band 2, keepalives (empty band-1 packets) while it works.
pub(super) const SIDE_BAND: &str = "side-band-64k";

/// The first (and only) command pkt-line of a plain single-ref push:
/// `<old> <new> <full-ref>\0<capabilities>`. `old` is the zero id when the remote
/// does not have the ref yet. `side_band` requests [`SIDE_BAND`], which a client may only
/// do when the server advertised it.
pub(super) fn command_line(
    old: Option<ObjectId>,
    new: ObjectId,
    full_ref: &str,
    side_band: bool,
) -> Vec<u8> {
    let zero = ObjectId::null(new.kind());
    let old = old.unwrap_or(zero);
    let mut line = format!("{old} {new} {full_ref}").into_bytes();
    line.push(0);
    line.extend_from_slice(b"report-status ");
    if side_band {
        line.extend_from_slice(SIDE_BAND.as_bytes());
        line.push(b' ');
    }
    line.extend_from_slice(b"agent=rc-git/");
    line.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
    line
}

/// How the server answered a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Report {
    /// The pack was unpacked and the ref updated.
    Ok,
    /// The server refused: `unpack` failed or the ref update was rejected with the
    /// given reason (e.g. `non-fast-forward`).
    Refused {
        /// The server's reason, verbatim.
        reason: String,
        /// Whether the reason says the remote ref is not where the push expected it: a
        /// non-fast-forward, or a ref someone else moved or created since the advertisement.
        remote_moved: bool,
    },
}

/// Splits the band-1 bytes of a sideband reply into the payloads of the pkt-lines framed
/// inside them: with [`SIDE_BAND`] the report is a pkt-line stream sent as band-1 data,
/// so it carries a second layer of framing the transport does not strip. Flush, delimiter
/// and response-end packets carry no payload and are skipped.
pub(super) fn report_lines(band1: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut rest = band1;
    let mut lines = Vec::new();
    while !rest.is_empty() {
        let (head, tail) = rest
            .split_at_checked(4)
            .ok_or("the server's report ends inside a pkt-line length")?;
        let len = std::str::from_utf8(head)
            .ok()
            .and_then(|hex| usize::from_str_radix(hex, 16).ok())
            .ok_or_else(|| {
                format!(
                    "the server's report has a bad pkt-line length '{}'",
                    head.escape_ascii()
                )
            })?;
        let Some(payload_len) = len.checked_sub(4) else {
            rest = tail;
            continue;
        };
        let payload = tail
            .get(..payload_len)
            .ok_or("the server's report ends inside a pkt-line")?;
        lines.push(payload);
        rest = tail.get(payload_len..).unwrap_or_default();
    }
    Ok(lines)
}

/// Parses a `report-status` reply. `lines` are its pkt-line payloads, one per pkt-line
/// with framing stripped, each with or without its trailing LF (a receiver must accept
/// both); `full_ref` is the one ref pushed.
pub(super) fn parse_report<'a>(
    lines: impl IntoIterator<Item = &'a [u8]>,
    full_ref: &str,
) -> Result<Report, String> {
    let mut unpack_ok = false;
    let mut ref_status: Option<Report> = None;
    let mut seen = Vec::new();
    for line in lines {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        keep_excerpt(&mut seen, line);
        let line = String::from_utf8_lossy(line);
        let line = line.as_ref();
        if let Some(message) = line.strip_prefix("ERR ") {
            return Err(format!(
                "the server reported an error: {}",
                message.escape_debug()
            ));
        } else if let Some(status) = line.strip_prefix("unpack ") {
            if status == "ok" {
                unpack_ok = true;
            } else {
                return Ok(Report::Refused {
                    reason: format!("unpack {status}"),
                    remote_moved: false,
                });
            }
        } else if let Some(rest) = line.strip_prefix("ok ") {
            if rest == full_ref {
                ref_status = Some(Report::Ok);
            }
        } else if let Some(rest) = line.strip_prefix("ng ") {
            let (name, reason) = rest.split_once(' ').unwrap_or((rest, "rejected"));
            if name == full_ref {
                ref_status = Some(Report::Refused {
                    reason: reason.to_owned(),
                    remote_moved: names_a_moved_remote(reason),
                });
            }
        }
    }
    if !unpack_ok {
        return Err(format!(
            "the server sent no 'unpack' status; its reply was '{}'",
            excerpt(&seen)
        ));
    }
    ref_status.ok_or_else(|| {
        format!(
            "the server sent no status for '{full_ref}'; its reply was '{}'",
            excerpt(&seen)
        )
    })
}

/// How much of a report-status reply an error quotes: enough to show what the server
/// said instead, bounded because the reply is untrusted.
const EXCERPT_LIMIT: usize = 200;

/// Appends one report line, LF-terminated, to `seen` without growing it past
/// [`EXCERPT_LIMIT`] bytes.
fn keep_excerpt(seen: &mut Vec<u8>, line: &[u8]) {
    for byte in line.iter().chain(b"\n") {
        if seen.len() >= EXCERPT_LIMIT {
            return;
        }
        seen.push(*byte);
    }
}

/// The kept reply as text, with line feeds and other control characters escaped.
fn excerpt(seen: &[u8]) -> String {
    String::from_utf8_lossy(seen).escape_debug().to_string()
}

/// The reasons a server gives when the remote ref is not where the push expected it.
/// `non-fast-forward` and `fetch first` come from servers that check ancestry themselves;
/// git's `receive-pack` answers `incorrect old value provided` when the ref moved since the
/// advertisement, and `reference already exists` when a new branch was created meanwhile.
/// Older servers' `failed to update ref` is left out: it also covers unrelated failures.
const REMOTE_MOVED_REASONS: [&str; 4] = [
    "non-fast-forward",
    "fetch first",
    "incorrect old value provided",
    "reference already exists",
];

fn names_a_moved_remote(reason: &str) -> bool {
    REMOTE_MOVED_REASONS
        .iter()
        .any(|moved| reason.contains(moved))
}

/// How much band-2 text is kept: the last lines whose total stays within this many bytes.
/// A server's stderr is untrusted and unbounded; the end of it is where a hook or
/// `receive-pack` says why it refused.
const MESSAGES_LIMIT: usize = 4 * 1024;

/// The server's band-2 messages (hook output, `receive-pack` errors), as a terminal would
/// show them: a line ended by `\r` is overwritten by the next one, so progress meters
/// collapse to their last state, and every other control character is escaped. Bounded to
/// the last [`MESSAGES_LIMIT`] bytes.
#[derive(Debug, Default)]
pub(super) struct ServerMessages {
    lines: VecDeque<String>,
    bytes: usize,
    dropped: bool,
    pending: String,
    returned: bool,
}

impl ServerMessages {
    /// Adds one band-2 packet. The transport strips a packet's trailing newline, so the
    /// packet's end also ends a line.
    pub(super) fn push(&mut self, text: &[u8]) {
        for ch in String::from_utf8_lossy(text).chars() {
            match ch {
                '\n' => self.end_line(),
                '\r' => self.returned = true,
                ch => {
                    if self.returned {
                        self.pending.clear();
                        self.returned = false;
                    }
                    if self.pending.len() < MESSAGES_LIMIT {
                        if ch.is_control() {
                            self.pending.extend(ch.escape_default());
                        } else {
                            self.pending.push(ch);
                        }
                    }
                }
            }
        }
        self.end_line();
    }

    fn end_line(&mut self) {
        self.returned = false;
        let line = std::mem::take(&mut self.pending);
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        self.bytes += line.len();
        self.lines.push_back(line.to_owned());
        while self.bytes > MESSAGES_LIMIT {
            let Some(oldest) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= oldest.len();
            self.dropped = true;
        }
    }

    /// The kept lines, each prefixed `remote: ` as git prints them, one per line; `None`
    /// when the server said nothing.
    pub(super) fn text(&self) -> Option<String> {
        if self.lines.is_empty() {
            return None;
        }
        let mut text = String::new();
        if self.dropped {
            text.push_str("remote: …");
        }
        for line in &self.lines {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("remote: ");
            text.push_str(line);
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payloads of a reply without a side band: one pkt-line per report line.
    fn lines(payload: &[u8]) -> Vec<&[u8]> {
        payload.split(|byte| *byte == b'\n').collect()
    }

    fn oid(byte: u8) -> ObjectId {
        ObjectId::try_from(&[byte; 20][..]).expect("20 bytes")
    }

    #[test]
    fn command_line_uses_the_zero_id_for_a_new_ref() {
        let line = command_line(None, oid(0xab), "refs/heads/main", false);
        let text = String::from_utf8_lossy(&line);
        assert!(text.starts_with(&format!(
            "{} {} refs/heads/main\0",
            "0".repeat(40),
            "ab".repeat(20)
        )));
        assert!(text.contains("report-status agent=rc-git/"));
    }

    #[test]
    fn command_line_requests_the_side_band_only_when_asked() {
        let line = command_line(None, oid(0xab), "refs/heads/main", true);
        let text = String::from_utf8_lossy(&line);
        assert!(
            text.contains("\0report-status side-band-64k agent=rc-git/"),
            "{text}"
        );
    }

    #[test]
    fn command_line_carries_the_old_id() {
        let line = command_line(Some(oid(0x01)), oid(0x02), "refs/heads/topic", false);
        assert!(String::from_utf8_lossy(&line).starts_with(&format!(
            "{} {} refs/heads/topic\0",
            "01".repeat(20),
            "02".repeat(20)
        )));
    }

    #[test]
    fn a_successful_report_parses() {
        let report = parse_report(lines(b"unpack ok\nok refs/heads/main\n"), "refs/heads/main");
        assert_eq!(report, Ok(Report::Ok));
    }

    #[test]
    fn a_report_whose_lines_lack_their_line_feeds_parses() {
        let report = parse_report(
            [&b"unpack ok"[..], b"ok refs/heads/main"],
            "refs/heads/main",
        );
        assert_eq!(report, Ok(Report::Ok));
    }

    #[test]
    fn a_rejection_carries_the_reason() {
        let report = parse_report(
            lines(b"unpack ok\nng refs/heads/main non-fast-forward\n"),
            "refs/heads/main",
        );
        assert_eq!(
            report,
            Ok(Report::Refused {
                reason: "non-fast-forward".to_owned(),
                remote_moved: true,
            })
        );
    }

    fn refusal(reason: &str) -> Result<Report, String> {
        let reply = format!("unpack ok\nng refs/heads/main {reason}\n");
        parse_report(lines(reply.as_bytes()), "refs/heads/main")
    }

    fn moved(reason: &str) -> Report {
        Report::Refused {
            reason: reason.to_owned(),
            remote_moved: true,
        }
    }

    #[test]
    fn fetch_first_means_the_remote_moved() {
        assert_eq!(refusal("fetch first"), Ok(moved("fetch first")));
    }

    #[test]
    fn incorrect_old_value_provided_means_the_remote_moved() {
        assert_eq!(
            refusal("incorrect old value provided"),
            Ok(moved("incorrect old value provided"))
        );
    }

    #[test]
    fn reference_already_exists_means_the_remote_moved() {
        assert_eq!(
            refusal("reference already exists"),
            Ok(moved("reference already exists"))
        );
    }

    #[test]
    fn failed_to_update_ref_stays_a_plain_refusal() {
        assert_eq!(
            refusal("failed to update ref"),
            Ok(Report::Refused {
                reason: "failed to update ref".to_owned(),
                remote_moved: false,
            })
        );
    }

    #[test]
    fn an_unpack_failure_is_refused() {
        let report = parse_report(lines(b"unpack index-pack failed\n"), "refs/heads/main");
        assert_eq!(
            report,
            Ok(Report::Refused {
                reason: "unpack index-pack failed".to_owned(),
                remote_moved: false,
            })
        );
    }

    #[test]
    fn a_report_without_our_ref_is_an_error() {
        assert!(
            parse_report(
                lines(b"unpack ok\nok refs/heads/other\n"),
                "refs/heads/main"
            )
            .is_err()
        );
        assert!(parse_report(lines(b""), "refs/heads/main").is_err());
    }

    #[test]
    fn an_err_line_carries_the_server_message() {
        let error = parse_report(lines(b"ERR access denied\n"), "refs/heads/main")
            .expect_err("an ERR line is an error");
        assert_eq!(error, "the server reported an error: access denied");
    }

    #[test]
    fn a_missing_ref_status_quotes_the_reply() {
        let error = parse_report(
            lines(b"unpack ok\nok refs/heads/other\n"),
            "refs/heads/main",
        )
        .expect_err("no status for our ref");
        assert!(error.contains("refs/heads/main"), "{error}");
        assert!(error.contains("refs/heads/other"), "{error}");
    }

    #[test]
    fn a_missing_unpack_status_quotes_a_bounded_escaped_reply() {
        let reply = format!("\x1b[31m{}", "x".repeat(1000));
        let error =
            parse_report([reply.as_bytes()], "refs/heads/main").expect_err("no unpack status");
        assert!(error.contains("\\u{1b}[31mxxx"), "{error}");
        assert!(!error.contains('\x1b'), "{error}");
        assert!(error.len() < EXCERPT_LIMIT + 100, "{}", error.len());
    }

    #[test]
    fn a_hook_rejection_without_reason_still_parses() {
        let report = parse_report(lines(b"unpack ok\nng refs/heads/main\n"), "refs/heads/main");
        assert_eq!(
            report,
            Ok(Report::Refused {
                reason: "rejected".to_owned(),
                remote_moved: false,
            })
        );
    }

    #[test]
    fn a_side_band_report_decodes_its_nested_pkt_lines() {
        let band1 = b"000eunpack ok\n0017ok refs/heads/main\n0000";
        let decoded = report_lines(band1).expect("decode");
        assert_eq!(
            decoded,
            vec![&b"unpack ok\n"[..], &b"ok refs/heads/main\n"[..]]
        );
        assert_eq!(parse_report(decoded, "refs/heads/main"), Ok(Report::Ok));
    }

    #[test]
    fn a_side_band_rejection_carries_the_reason() {
        let band1 = b"000eunpack ok\n0032ng refs/heads/trunk pre-receive hook declined\n0000";
        let decoded = report_lines(band1).expect("decode");
        assert_eq!(
            parse_report(decoded, "refs/heads/trunk"),
            Ok(Report::Refused {
                reason: "pre-receive hook declined".to_owned(),
                remote_moved: false,
            })
        );
    }

    #[test]
    fn a_truncated_side_band_report_is_an_error() {
        assert!(report_lines(b"000eunpack").is_err());
        assert!(report_lines(b"00").is_err());
        assert!(report_lines(b"zzzzunpack ok\n").is_err());
    }

    #[test]
    fn server_messages_keep_the_last_state_of_a_progress_line() {
        let mut messages = ServerMessages::default();
        messages.push(b"Resolving deltas:  50%\rResolving deltas: 100%, done.");
        messages.push(b"refused by policy");
        assert_eq!(
            messages.text().as_deref(),
            Some("remote: Resolving deltas: 100%, done.\nremote: refused by policy")
        );
    }

    #[test]
    fn server_messages_keep_a_crlf_line_and_escape_control_characters() {
        let mut messages = ServerMessages::default();
        messages.push(b"denied\r\n\x1b[31mred\x07");
        assert_eq!(
            messages.text().as_deref(),
            Some("remote: denied\nremote: \\u{1b}[31mred\\u{7}")
        );
    }

    #[test]
    fn server_messages_are_bounded_to_their_tail() {
        let mut messages = ServerMessages::default();
        for _ in 0..1000 {
            messages.push(b"noise noise noise");
        }
        messages.push(b"the real reason");
        let text = messages.text().expect("text");
        assert!(text.len() < 2 * MESSAGES_LIMIT, "{}", text.len());
        assert!(text.starts_with("remote: …\n"), "{text}");
        assert!(text.ends_with("remote: the real reason"), "{text}");
    }

    #[test]
    fn silence_has_no_server_messages() {
        let mut messages = ServerMessages::default();
        messages.push(b"\r");
        assert_eq!(messages.text(), None);
    }
}
