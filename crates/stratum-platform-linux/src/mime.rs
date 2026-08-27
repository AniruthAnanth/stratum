//! Desktop entries, MIME packages and `mimeapps.list` — 08 §6.3.
//!
//! Pure text, no I/O, so the whole of §6.3's Linux half is asserted from any
//! host. [`crate::shell`] is the part that writes what this module produces.
//!
//! # The rule that shapes every function here
//!
//! §6.3, from a measured fact: **Stata already declares itself the default
//! handler** for `.do`, `.ado` and `.dta`. A user with Stata installed must not
//! have their double-click behaviour change because they installed Stratum. So
//! registering is `[Added Associations]` — "Stratum can open this" — and
//! becoming the default is `[Default Applications]`, which only ever happens
//! through an explicit action in Settings → General → File Associations.
//! [`MimeApps::add_association`] and [`MimeApps::set_default`] are separate
//! functions for exactly that reason, and the first one never touches the
//! second one's group.
//!
//! # Why `mimeapps.list` is edited rather than rewritten
//!
//! It holds every default the user has ever set, for every application on the
//! machine. Parsing it into a map and serialising the map back loses comments,
//! loses group order, and loses any group a future spec adds. This module keeps
//! the file as lines and rewrites exactly the one line it must.

use camino::Utf8Path;

/// The desktop entry's file name. Reverse-DNS as the freedesktop
/// Desktop Entry Specification requires, so the entry, the metainfo file and
/// the Flatpak id all agree.
pub const DESKTOP_FILE: &str = "dev.stratum.Stratum.desktop";

/// The shared-mime-info package file name.
pub const MIME_PACKAGE_FILE: &str = "dev.stratum.stratum.xml";

/// Our MIME type for `.do` and `.ado`. freedesktop ships none for Stata, so
/// §6.3 defines these two.
pub const MIME_DO: &str = "text/x-stata-do";

/// Our MIME type for `.dta`.
pub const MIME_DTA: &str = "application/x-stata-dta";

/// The `stratum://` scheme handler, used for AI provider OAuth callbacks
/// (§21/§22) and `stratum://open?file=…&line=…` deep links.
pub const MIME_SCHEME: &str = "x-scheme-handler/stratum";

/// Every MIME type the desktop entry claims, in the order it writes them.
pub const MIME_TYPES: [&str; 3] = [MIME_DO, MIME_DTA, MIME_SCHEME];

/// The MIME type for a filename extension, dot optional. `None` for anything
/// we do not claim — `.smcl` is deliberately absent, because we render SMCL but
/// do not want to be offered as an editor for it.
#[must_use]
pub fn mime_for_extension(ext: &str) -> Option<&'static str> {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "do" | "ado" => Some(MIME_DO),
        "dta" => Some(MIME_DTA),
        _ => None,
    }
}

/// The `shared-mime-info` package, transcribed from 08 §6.3.
///
/// The `.dta` magic is not a guess: it was verified against the licensed
/// installation's `auto.dta`, which begins with the literal ASCII
/// `<stata_dta>`. The two low-priority byte matches cover the pre-Stata-13
/// formats, where the first byte is the release code.
#[must_use]
pub const fn mime_package_xml() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="text/x-stata-do">
    <comment>Stata do-file</comment>
    <sub-class-of type="text/plain"/>
    <glob pattern="*.do"/>
    <glob pattern="*.ado"/>
  </mime-type>
  <mime-type type="application/x-stata-dta">
    <comment>Stata dataset</comment>
    <glob pattern="*.dta"/>
    <magic priority="80"><match type="string" offset="0" value="&lt;stata_dta&gt;"/></magic>
    <magic priority="40"><match type="byte" offset="0" value="0x71"/></magic>
    <magic priority="40"><match type="byte" offset="0" value="0x72"/></magic>
  </mime-type>
</mime-info>
"#
}

/// The `.desktop` entry for a build that has to install its own — an AppImage
/// or an unpacked tarball. The `.deb` and `.rpm` ship the same content as a
/// packaged file (§6.1), which is why [`crate::Packaging::owns_desktop_integration`]
/// exists: writing this into `~/.local/share` on top of a packaged install
/// gives the user two entries in their launcher.
///
/// `%U` and not `%F`: `MimeType` includes `x-scheme-handler/stratum`, and a
/// scheme handler is invoked with a URI, which `%F` cannot carry.
/// `StartupWMClass` is what lets the shell match the running window to this
/// entry instead of showing a second, generic icon in the dock.
#[must_use]
pub fn desktop_entry(exec: &Utf8Path) -> String {
    let mime = MIME_TYPES.join(";");
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Stratum\n\
         GenericName=Statistical IDE\n\
         Comment=Interactive statistical IDE\n\
         Exec={exec} %U\n\
         Icon=dev.stratum.Stratum\n\
         Terminal=false\n\
         Categories=Development;Science;Math;IDE;\n\
         Keywords=Stata;statistics;data;analysis;do-file;\n\
         MimeType={mime};\n\
         StartupNotify=true\n\
         StartupWMClass=Stratum\n"
    )
}

/// One line of a `mimeapps.list`, kept in its original form.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Line {
    /// `[Default Applications]`.
    Group(String),
    /// `key=value`, with the key trimmed for lookup and the value as written.
    Entry { key: String, value: String },
    /// A comment, a blank line, or anything we do not understand. Preserved
    /// byte for byte.
    Verbatim(String),
}

/// A `mimeapps.list`, edited in place.
///
/// The freedesktop "Association between MIME types and applications" spec puts
/// three groups in this file: `[Added Associations]` (this application can
/// open the type), `[Removed Associations]` (it must not be offered), and
/// `[Default Applications]` (it is the default). Only the first and third
/// matter to us, and they must never be confused — see the module docs.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MimeApps {
    lines: Vec<Line>,
}

/// The `[Default Applications]` group name.
const GROUP_DEFAULT: &str = "Default Applications";
/// The `[Added Associations]` group name.
const GROUP_ADDED: &str = "Added Associations";

impl MimeApps {
    /// Parse. An unparseable file is not an error: `mimeapps.list` is
    /// hand-edited by users and by half a dozen desktop environments, and
    /// refusing to add one association because someone left a stray line in it
    /// would be worse than preserving the line.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut lines = Vec::new();
        for raw in text.lines() {
            let t = raw.trim();
            if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                lines.push(Line::Group(name.trim().to_owned()));
            } else if !t.starts_with('#') && !t.is_empty() && t.contains('=') {
                // `split_once` and not `split('=')`: a desktop file id cannot
                // contain `=`, but a value list can, and losing the tail would
                // silently drop the user's other handlers.
                let Some((k, v)) = t.split_once('=') else {
                    lines.push(Line::Verbatim(raw.to_owned()));
                    continue;
                };
                lines.push(Line::Entry {
                    key: k.trim().to_owned(),
                    value: v.trim().to_owned(),
                });
            } else {
                lines.push(Line::Verbatim(raw.to_owned()));
            }
        }
        Self { lines }
    }

    /// Serialise. Round-trips a file this never touched, modulo a trailing
    /// newline.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                Line::Group(name) => out.push_str(&format!("[{name}]")),
                Line::Entry { key, value } => out.push_str(&format!("{key}={value}")),
                Line::Verbatim(raw) => out.push_str(raw),
            }
            out.push('\n');
        }
        out
    }

    /// The desktop file id that currently opens `mime` by default, from
    /// `[Default Applications]`. `None` when the file does not say.
    ///
    /// The value is a `;`-separated preference list; the first entry wins, and
    /// the rest are what the desktop falls back to if the first one is not
    /// installed.
    #[must_use]
    pub fn default_for(&self, mime: &str) -> Option<&str> {
        self.value_in(GROUP_DEFAULT, mime)
            .and_then(|v| v.split(';').map(str::trim).find(|s| !s.is_empty()))
    }

    /// Whether `desktop_id` is listed under `[Added Associations]` for `mime`.
    #[must_use]
    pub fn is_associated(&self, mime: &str, desktop_id: &str) -> bool {
        self.value_in(GROUP_ADDED, mime)
            .is_some_and(|v| v.split(';').map(str::trim).any(|s| s == desktop_id))
    }

    /// Offer `desktop_id` as *a* handler for `mime`, leaving the default alone.
    /// This is §6.3's "register as a capable alternative and never steal the
    /// default". Idempotent.
    pub fn add_association(&mut self, mime: &str, desktop_id: &str) {
        if self.is_associated(mime, desktop_id) {
            return;
        }
        let existing = self.value_in(GROUP_ADDED, mime).unwrap_or_default();
        let mut ids: Vec<&str> = existing
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        ids.push(desktop_id);
        let value = format!("{};", ids.join(";"));
        self.set_in(GROUP_ADDED, mime, &value);
    }

    /// Make `desktop_id` the default for `mime`. Only ever from an explicit
    /// user action (§6.3).
    ///
    /// The previous default is kept as the second entry in the preference list
    /// rather than dropped: if the user later uninstalls Stratum, their old
    /// handler comes back by itself instead of the type becoming unhandled.
    pub fn set_default(&mut self, mime: &str, desktop_id: &str) {
        let existing = self.value_in(GROUP_DEFAULT, mime).unwrap_or_default();
        let mut ids: Vec<&str> = existing
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != desktop_id)
            .collect();
        ids.insert(0, desktop_id);
        let value = ids.join(";");
        self.set_in(GROUP_DEFAULT, mime, &value);
    }

    fn value_in(&self, group: &str, key: &str) -> Option<&str> {
        let mut in_group = false;
        for line in &self.lines {
            match line {
                Line::Group(name) => in_group = name == group,
                Line::Entry { key: k, value } if in_group && k == key => {
                    return Some(value.as_str())
                }
                _ => {}
            }
        }
        None
    }

    /// Replace `key` inside `group`, creating either if absent. The insert goes
    /// at the END of the group's existing entries so unrelated lines keep their
    /// position and a diff of the file shows one changed line.
    fn set_in(&mut self, group: &str, key: &str, value: &str) {
        let mut group_start: Option<usize> = None;
        let mut group_end = self.lines.len();
        let mut in_group = false;
        for (i, line) in self.lines.iter().enumerate() {
            match line {
                Line::Group(name) if name == group => {
                    in_group = true;
                    group_start = Some(i);
                }
                Line::Group(_) if in_group => {
                    group_end = i;
                    in_group = false;
                }
                _ => {}
            }
        }
        let Some(start) = group_start else {
            // No such group. Append it, with a blank line before it when the
            // file is not empty, so we do not weld ourselves onto the last
            // group's final entry.
            if !self.lines.is_empty() {
                self.lines.push(Line::Verbatim(String::new()));
            }
            self.lines.push(Line::Group(group.to_owned()));
            self.lines.push(Line::Entry {
                key: key.to_owned(),
                value: value.to_owned(),
            });
            return;
        };

        for i in start + 1..group_end.max(start + 1) {
            if let Some(Line::Entry { key: k, value: v }) = self.lines.get_mut(i) {
                if k == key {
                    *v = value.to_owned();
                    return;
                }
            }
        }
        // Insert after the group's last non-blank line, not at `group_end`,
        // which may be several blank lines past it.
        let mut at = group_end;
        while at > start + 1
            && matches!(self.lines.get(at - 1), Some(Line::Verbatim(v)) if v.trim().is_empty())
        {
            at -= 1;
        }
        self.lines.insert(
            at,
            Line::Entry {
                key: key.to_owned(),
                value: value.to_owned(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "\
[Added Associations]
text/html=firefox.desktop;
application/pdf=org.gnome.Evince.desktop;

# the user set this by hand
[Default Applications]
text/html=firefox.desktop
application/pdf=org.gnome.Evince.desktop
";

    #[test]
    fn a_file_we_did_not_touch_round_trips() {
        assert_eq!(MimeApps::parse(REAL).to_text(), REAL);
    }

    /// The whole of §6.3 in one assertion: registering must not move the
    /// default, because a user with Stata installed keeps Stata.
    #[test]
    fn registering_never_changes_the_default() {
        let mut m = MimeApps::parse(REAL);
        m.add_association(MIME_DO, DESKTOP_FILE);
        m.add_association("text/html", DESKTOP_FILE);

        assert!(m.is_associated(MIME_DO, DESKTOP_FILE));
        assert!(m.is_associated("text/html", DESKTOP_FILE));
        assert_eq!(m.default_for("text/html"), Some("firefox.desktop"));
        assert_eq!(m.default_for(MIME_DO), None);
        // firefox is still offered as well; we were appended, not substituted.
        let text = m.to_text();
        assert!(text.contains("text/html=firefox.desktop;dev.stratum.Stratum.desktop;"));
    }

    #[test]
    fn registering_twice_changes_nothing() {
        let mut m = MimeApps::parse(REAL);
        m.add_association(MIME_DTA, DESKTOP_FILE);
        let once = m.to_text();
        m.add_association(MIME_DTA, DESKTOP_FILE);
        assert_eq!(m.to_text(), once);
    }

    /// Taking the default is an explicit user action, and it must be
    /// reversible: the handler we displaced stays in the preference list, so
    /// uninstalling Stratum restores it rather than leaving the type orphaned.
    #[test]
    fn taking_the_default_keeps_the_displaced_handler_behind_us() {
        let mut m = MimeApps::parse(REAL);
        m.set_default("text/html", DESKTOP_FILE);
        assert_eq!(m.default_for("text/html"), Some(DESKTOP_FILE));
        assert!(m
            .to_text()
            .contains("text/html=dev.stratum.Stratum.desktop;firefox.desktop"));
    }

    #[test]
    fn setting_the_same_default_twice_does_not_duplicate_us() {
        let mut m = MimeApps::parse(REAL);
        m.set_default(MIME_DO, DESKTOP_FILE);
        m.set_default(MIME_DO, DESKTOP_FILE);
        assert!(m.to_text().contains(&format!("{MIME_DO}={DESKTOP_FILE}\n")));
    }

    #[test]
    fn a_missing_group_is_created_without_welding_onto_the_previous_one() {
        let mut m = MimeApps::parse("[Default Applications]\ntext/html=firefox.desktop\n");
        m.add_association(MIME_DO, DESKTOP_FILE);
        let text = m.to_text();
        assert!(text.contains("\n[Added Associations]\n"), "{text}");
        assert!(text.starts_with("[Default Applications]\ntext/html=firefox.desktop\n"));
    }

    #[test]
    fn an_empty_file_gets_exactly_one_group() {
        let mut m = MimeApps::parse("");
        m.add_association(MIME_DO, DESKTOP_FILE);
        assert_eq!(
            m.to_text(),
            format!("[Added Associations]\n{MIME_DO}={DESKTOP_FILE};\n")
        );
    }

    #[test]
    fn extensions_map_to_the_two_types_we_claim_and_nothing_else() {
        assert_eq!(mime_for_extension("do"), Some(MIME_DO));
        assert_eq!(mime_for_extension(".DO"), Some(MIME_DO));
        assert_eq!(mime_for_extension("ado"), Some(MIME_DO));
        assert_eq!(mime_for_extension("dta"), Some(MIME_DTA));
        // We render SMCL but do not want to be offered as its editor.
        assert_eq!(mime_for_extension("smcl"), None);
        assert_eq!(mime_for_extension("txt"), None);
    }

    /// `%U`, not `%F`: the entry also handles `x-scheme-handler/stratum`, and a
    /// scheme handler is invoked with a URI.
    #[test]
    fn the_desktop_entry_carries_a_uri_placeholder_and_every_mime_type() {
        let entry = desktop_entry(Utf8Path::new("/usr/bin/stratum"));
        assert!(entry.contains("Exec=/usr/bin/stratum %U\n"));
        assert!(entry.contains(&format!("MimeType={MIME_DO};{MIME_DTA};{MIME_SCHEME};\n")));
        assert!(entry.contains("StartupWMClass=Stratum\n"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
    }

    /// The magic is the one thing in the MIME package that came from a
    /// measurement rather than from a spec, and it is irreplaceable — the Stata
    /// licence it was captured under has expired.
    #[test]
    fn the_mime_package_keeps_the_verified_dta_magic() {
        let xml = mime_package_xml();
        assert!(xml.contains(r#"value="&lt;stata_dta&gt;""#));
        assert!(xml.contains(r#"<glob pattern="*.ado"/>"#));
        assert!(xml.contains(r#"<sub-class-of type="text/plain"/>"#));
    }
}
