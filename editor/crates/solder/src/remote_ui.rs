//! The SSH picker only parses a destination. Its subscriber owns connecting
//! off the UI thread and opening the resulting project window.

use std::net::Ipv6Addr;

use gpui::{
    AnyElement, Context, DismissEvent, EventEmitter, SharedString, Task, Window, actions, div,
    prelude::*,
};

use crate::{
    lang::{t, tf},
    picker::{Picker, PickerDelegate},
    remote::{Endpoint, Target},
    theme::{ActiveTheme, UI_FONT_SIZE},
};

actions!(remote_ui, [ConnectSsh]);

#[derive(Clone, Debug)]
pub enum SshEvent {
    Accepted(Target),
}

impl EventEmitter<SshEvent> for Picker<SshInput> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Format,
    Host,
    Port,
    Path,
    Encoding,
    Controls,
}

impl InputError {
    fn message(self) -> &'static str {
        match self {
            Self::Format => t("Use ssh://user@host:port/path or alias:/path"),
            Self::Host => t("Enter a host or SSH alias without passwords or options"),
            Self::Port => t("Enter a port from 1 to 65535"),
            Self::Path => t("Enter an absolute project path beginning with /"),
            Self::Encoding => t("Use valid percent escapes and UTF-8 in the URI path"),
            Self::Controls => t("Remove control characters from the connection"),
        }
    }
}

/// A single SSH URI, or an SSH configuration alias with an absolute path.
/// Alias paths stay literal; only URI paths are percent decoded. No IO.
pub fn parse_target(input: &str) -> Result<Target, InputError> {
    if input.chars().any(char::is_control) {
        return Err(InputError::Controls);
    }
    let (host, port, path) = if let Some(uri) = input.strip_prefix("ssh://") {
        let slash = uri.find('/').ok_or(InputError::Path)?;
        let (authority, path) = uri.split_at(slash);
        let (host, port) = authority_parts(authority)?;
        if path.chars().any(char::is_whitespace) || path.contains(['?', '#', '\\']) {
            return Err(InputError::Encoding);
        }
        (host, port, decode_path(path)?)
    } else {
        let (alias, path) = input.split_once(':').ok_or(InputError::Format)?;
        if path.starts_with("//") {
            return Err(InputError::Format);
        }
        if !name(alias) {
            return Err(InputError::Host);
        }
        (alias.to_owned(), 22, path.to_owned())
    };
    if !path.starts_with('/') {
        return Err(InputError::Path);
    }
    if path.chars().any(char::is_control) {
        return Err(InputError::Controls);
    }
    let endpoint = Endpoint::Ssh { host, port };
    endpoint.validate().map_err(|_| InputError::Host)?;
    Ok(Target { endpoint, path })
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

fn authority_parts(authority: &str) -> Result<(String, u16), InputError> {
    let (user, address) = match authority.split_once('@') {
        Some((user, address)) if name(user) => (Some(user), address),
        Some(_) => return Err(InputError::Host),
        None => (None, authority),
    };
    let (host, suffix) = if let Some(ip) = address.strip_prefix('[') {
        let end = ip.find(']').ok_or(InputError::Host)?;
        ip[..end]
            .parse::<Ipv6Addr>()
            .map_err(|_| InputError::Host)?;
        (&address[..end + 2], &ip[end + 1..])
    } else {
        let (host, suffix) = address
            .find(':')
            .map_or((address, ""), |at| address.split_at(at));
        if !name(host) {
            return Err(InputError::Host);
        }
        (host, suffix)
    };
    let port = if suffix.is_empty() {
        22
    } else {
        let port = suffix.strip_prefix(':').ok_or(InputError::Host)?;
        if port.is_empty() || !port.bytes().all(|c| c.is_ascii_digit()) {
            return Err(InputError::Port);
        }
        port.parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or(InputError::Port)?
    };
    Ok((
        user.map_or_else(|| host.to_owned(), |user| format!("{user}@{host}")),
        port,
    ))
}

fn decode_path(path: &str) -> Result<String, InputError> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(hex).ok_or(InputError::Encoding)?;
            let low = bytes.next().and_then(hex).ok_or(InputError::Encoding)?;
            decoded.push(high * 16 + low);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).map_err(|_| InputError::Encoding)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Default)]
pub struct SshInput {
    target: Option<Target>,
    error: Option<InputError>,
}

impl SshInput {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PickerDelegate for SshInput {
    fn placeholder(&self) -> SharedString {
        t("SSH connection: ssh://user@host:port/path or alias:/path").into()
    }

    fn match_count(&self) -> usize {
        usize::from(self.target.is_some())
    }

    fn selected_index(&self) -> usize {
        0
    }

    fn set_selected_index(&mut self, _: usize, _: &mut Context<Picker<Self>>) {}

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.target = None;
        self.error = None;
        if !query.is_empty() {
            match parse_target(&query) {
                Ok(target) => self.target = Some(target),
                Err(error) => self.error = Some(error),
            }
        }
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(target) = self.target.take() {
            cx.emit(SshEvent::Accepted(target));
            cx.emit(DismissEvent);
        }
    }

    fn render_match(
        &self,
        _: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let label = self.target.as_ref().map(|target| {
            let Endpoint::Ssh { host, port } = &target.endpoint else {
                unreachable!("the SSH parser only produces SSH targets")
            };
            tf("Connect to {}:{}: {}", &[host, port, &target.path])
        });
        div()
            .w_full()
            .truncate()
            .text_size(UI_FONT_SIZE)
            .text_color(cx.theme().fg)
            .children(label)
            .into_any_element()
    }

    fn empty_text(&self) -> SharedString {
        self.error
            .map_or_else(
                || t("Enter an SSH connection and an absolute project path"),
                InputError::message,
            )
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &str, expected_host: &str, expected_port: u16, expected_path: &str) {
        let target = parse_target(input).unwrap();
        let Endpoint::Ssh { host, port } = target.endpoint else {
            panic!("expected SSH")
        };
        assert_eq!(host, expected_host);
        assert_eq!(port, expected_port);
        assert_eq!(target.path, expected_path);
    }

    #[test]
    fn accepts_aliases_and_keeps_their_paths_literal() {
        check("my-alias:/srv/a b/%20", "my-alias", 22, "/srv/a b/%20");
        check("staging:/", "staging", 22, "/");
    }

    #[test]
    fn decodes_only_uri_paths_and_preserves_utf8_and_plus() {
        check(
            "ssh://user@example.org:2222/srv/a%20b/%D1%8F+%25",
            "user@example.org",
            2222,
            "/srv/a b/я+%",
        );
        check("ssh://host/srv/%2520", "host", 22, "/srv/%20");
        check("ssh://host/", "host", 22, "/");
    }

    #[test]
    fn accepts_bracketed_ipv6_with_an_optional_user_and_port() {
        check(
            "ssh://user@[2001:db8::1]:2222/srv/app",
            "user@[2001:db8::1]",
            2222,
            "/srv/app",
        );
        check("ssh://[::1]/srv/app", "[::1]", 22, "/srv/app");
    }

    #[test]
    fn refuses_passwords_options_ambiguous_authorities_and_missing_paths() {
        for input in [
            "",
            "host",
            "host:relative",
            "host:",
            "ssh://host",
            "ssh:///path",
            "ssh://user:password@host/path",
            "ssh://user@@host/path",
            "ssh://@host/path",
            "ssh://-oProxyCommand/path",
            "-F:/path",
            "ssh://host -F config/path",
            "ssh://host:22:23/path",
            "ssh://2001:db8::1/path",
            "ssh://[broken]/path",
            "ssh://[::1]extra/path",
            "ssh://[::1/path",
            "https://host/path",
        ] {
            assert!(parse_target(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn refuses_invalid_ports_escapes_controls_queries_and_fragments() {
        for input in [
            "ssh://host:0/path",
            "ssh://host:65536/path",
            "ssh://host:/path",
            "ssh://host:+22/path",
            "ssh://host:abc/path",
            "ssh://host/a%",
            "ssh://host/a%2",
            "ssh://host/a%GG",
            "ssh://host/%FF",
            "ssh://host/%00",
            "ssh://host/%0A",
            "ssh://host/a b",
            "ssh://host/path?option=yes",
            "ssh://host/path#fragment",
            "host:/path\n",
            "host:/path\u{85}",
            "ssh://host\t/path",
        ] {
            assert!(parse_target(input).is_err(), "accepted {input:?}");
        }
    }

    #[gpui::test]
    fn invalid_input_cannot_confirm_a_previous_target(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::perf::Perf::new(std::time::Instant::now()));
            cx.set_global(crate::settings::Settings::default());
        });
        let (picker, cx) = cx.add_window_view(|window, cx| {
            crate::theme::put(crate::theme::Theme::dark(), window, cx);
            Picker::new(SshInput::new(), window, cx)
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = events.clone();
        let subscription = cx.update(|_, cx| {
            cx.subscribe(&picker, move |_, event: &SshEvent, _| {
                received.borrow_mut().push(event.clone())
            })
        });
        picker.update_in(cx, |picker, window, cx| {
            picker
                .delegate
                .update_matches("ssh://user@host:2222/srv/app".into(), window, cx)
                .detach();
            assert_eq!(picker.delegate.match_count(), 1);
            picker
                .delegate
                .update_matches("ssh://user:password@host/srv/app".into(), window, cx)
                .detach();
            assert_eq!(picker.delegate.match_count(), 0);
            picker.delegate.confirm(window, cx);
        });
        assert!(events.borrow().is_empty());
        picker.update_in(cx, |picker, window, cx| {
            picker
                .delegate
                .update_matches("alias:/srv/a b".into(), window, cx)
                .detach();
            picker.delegate.confirm(window, cx);
            picker.delegate.confirm(window, cx);
        });
        let events = events.borrow();
        assert_eq!(events.len(), 1);
        let SshEvent::Accepted(target) = &events[0];
        assert_eq!(target.path, "/srv/a b");
        drop(subscription);
    }
}
