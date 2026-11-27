//! Linux font fallback for NSH's separate text shaping and drawing contexts.

use std::collections::BTreeMap;

use nsh_core::prelude::*;

/// Font overrides needed by the current desktop's text renderer.
#[module]
pub struct DesktopFonts;

#[module]
impl DesktopFonts {
    /// Returns only the theme fonts whose system fallback loses Han glyphs when reloaded.
    pub fn fallbacks() -> BTreeMap<String, String> {
        #[cfg(target_os = "linux")]
        {
            linux::FontFallback::detect()
        }
        #[cfg(not(target_os = "linux"))]
        BTreeMap::new()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    use gtk4::glib::translate::ToGlibPtr;
    use gtk4::glib::types::Type;
    use pangocairo::pango::{
        Context, CoverageLevel, Font, FontDescription, Language, Weight,
    };
    use pangocairo::prelude::*;

    #[allow(unsafe_code)]
    #[link(name = "fontconfig")]
    unsafe extern "C" {
        fn FcConfigGetCurrent() -> *mut c_void;
        fn FcConfigParseAndLoadFromMemory(
            config: *mut c_void,
            buffer: *const c_char,
            complain: c_int,
        ) -> c_int;
    }

    #[allow(unsafe_code)]
    #[link(name = "pangoft2-1.0")]
    unsafe extern "C" {
        fn pango_fc_font_map_config_changed(fcfontmap: *mut c_void);
    }

    /// Mirrors NSH's font selection on the engine thread and face lookup on the drawing thread.
    pub(super) struct FontFallback {
        context: Context,
        language: Language,
        config_changed: bool,
    }

    impl FontFallback {
        /// Probes the two font roles independently once per process so healthy defaults stay in use.
        pub(super) fn detect() -> BTreeMap<String, String> {
            static DETECTED: OnceLock<BTreeMap<String, String>> =
                OnceLock::new();
            DETECTED
                .get_or_init(|| {
                    let mut probe = Self {
                        context: pangocairo::FontMap::new().create_context(),
                        language: Language::default(),
                        config_changed: false,
                    };
                    let mut overrides = BTreeMap::new();
                    for (token, generic) in [
                        ("font-sans", "sans-serif"),
                        ("font-mono", "monospace"),
                    ] {
                        if let Some(family) = probe.resolve(generic) {
                            overrides.insert(token.to_owned(), family);
                        }
                    }
                    if probe.config_changed {
                        Self::notify_default_font_map();
                    }
                    overrides
                })
                .clone()
        }

        /// Preserves the system's preferred CJK face when Fontconfig pattern rules hijack direct
        /// reloads, falling back to the highest-quality drawable CJK family otherwise.
        fn resolve(&mut self, generic: &str) -> Option<String> {
            let hijacked = {
                let fonts = self.fontset(generic)?;
                let shaped = fonts.font(u32::from('中'));
                if self.covers_han(&shaped) {
                    let description = shaped.describe_with_absolute_size();
                    let drawn = self.context.load_font(&description);
                    if drawn.as_ref().is_some_and(|font| {
                        font.describe_with_absolute_size() == description
                    }) {
                        return None;
                    }
                    let families = description
                        .family()
                        .zip(drawn.and_then(|font| font.describe().family()));
                    families.map(|(family, hijacker)| {
                        (family, hijacker, description)
                    })
                } else {
                    None
                }
            };

            // All `Fontset` and `Font` handles from the previous context are dropped before
            // appending to Fontconfig and recreating `self.context`.
            if let Some((family, hijacker, description)) = hijacked
                && hijacker != family
                && self.pin_family_in_fontconfig(&hijacker, &family)
                // Glyph indices belong to the original shaping face, even if another face also covers Han.
                && self.reloads_face(&description)
            {
                tracing::info!(
                    "pinned {} over {} in process fontconfig for desktop {} CJK rendering",
                    family,
                    hijacker,
                    generic
                );
                return None;
            }

            let fonts = self.fontset(generic)?;
            let mut best: Option<(u8, String)> = None;
            fonts.foreach(|_, candidate| {
                if self.draws_han(candidate)
                    && let Some(family) = candidate.describe().family()
                {
                    let name = family.to_string();
                    let score = self.candidate_score(&name);
                    if best
                        .as_ref()
                        .is_none_or(|(best_score, _)| score > *best_score)
                    {
                        best = Some((score, name));
                    }
                }
                false
            });
            let replacement = best.map(|(_, name)| name);
            match &replacement {
                Some(family) => tracing::info!(
                    "using {} for desktop {} CJK fallback",
                    family,
                    generic
                ),
                None => tracing::warn!(
                    "no drawable desktop {} CJK fallback found",
                    generic
                ),
            }
            replacement
        }

        /// Loads the Fontconfig-ordered fontset for a generic family role.
        fn fontset(&self, generic: &str) -> Option<pangocairo::pango::Fontset> {
            let mut description = FontDescription::new();
            description.set_family(generic);
            description.set_size(14 * pangocairo::pango::SCALE);
            self.context.load_fontset(&description, &self.language)
        }

        /// Appends an in-memory rule to the active Fontconfig configuration so direct reloads of
        /// `family` place `family` ahead of `hijacker`.
        fn pin_family_in_fontconfig(
            &mut self,
            hijacker: &str,
            family: &str,
        ) -> bool {
            let escaped_hijacker = Self::escape_xml(hijacker);
            let escaped_family = Self::escape_xml(family);
            let xml = format!(
                concat!(
                    "<?xml version=\"1.0\"?>",
                    "<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">",
                    "<fontconfig>",
                    "<match target=\"pattern\">",
                    "<test qual=\"first\" name=\"family\" compare=\"eq\"><string>{}</string></test>",
                    "<test name=\"family\" compare=\"eq\"><string>{}</string></test>",
                    "<edit name=\"family\" mode=\"prepend_first\" binding=\"strong\"><string>{}</string></edit>",
                    "</match>",
                    "</fontconfig>",
                ),
                escaped_hijacker, escaped_family, escaped_family,
            );
            let Ok(buffer) = CString::new(xml) else {
                return false;
            };
            // SAFETY: `FcConfigGetCurrent` returns the live process-wide Fontconfig configuration.
            #[allow(unsafe_code)]
            let config = unsafe { FcConfigGetCurrent() };
            if config.is_null() {
                return false;
            }
            // SAFETY: `config` is non-null and `buffer` is valid NUL-terminated XML.
            #[allow(unsafe_code)]
            let parsed = unsafe {
                FcConfigParseAndLoadFromMemory(config, buffer.as_ptr(), 0)
            } != 0;
            if !parsed {
                return false;
            }
            // A parsed rule changes the shared config even if it fails to restore this particular face.
            self.config_changed = true;
            self.context = pangocairo::FontMap::new().create_context();
            true
        }

        /// Flushes GTK's shared PangoFcFontMap on the GTK main loop after updating Fontconfig.
        fn notify_default_font_map() {
            if !gtk4::is_initialized() {
                return;
            }
            gtk4::glib::idle_add_once(|| {
                let default_map = pangocairo::FontMap::default();
                if Type::from_name("PangoFcFontMap")
                    .is_some_and(|fc_type| default_map.type_().is_a(fc_type))
                {
                    let ptr: *mut pangocairo::pango::ffi::PangoFontMap =
                        default_map.to_glib_none().0;
                    // SAFETY: Verified on the GTK main thread that `default_map` is a `PangoFcFontMap`.
                    #[allow(unsafe_code)]
                    unsafe {
                        pango_fc_font_map_config_changed(ptr.cast::<c_void>());
                    }
                }
            });
        }

        /// Escapes XML special characters before embedding a font family name into a rule.
        fn escape_xml(value: &str) -> String {
            let mut escaped = String::with_capacity(value.len());
            for ch in value.chars() {
                match ch {
                    '&' => escaped.push_str("&amp;"),
                    '<' => escaped.push_str("&lt;"),
                    '>' => escaped.push_str("&gt;"),
                    '"' => escaped.push_str("&quot;"),
                    '\'' => escaped.push_str("&apos;"),
                    _ => escaped.push(ch),
                }
            }
            escaped
        }

        /// Checks whether a face provides exact coverage for representative Han characters.
        fn covers_han(&self, font: &Font) -> bool {
            let coverage = font.coverage(&self.language);
            ['中', '文', '。']
                .into_iter()
                .all(|c| coverage.get(c as i32) == CoverageLevel::Exact)
        }

        /// A fallback must cover Chinese and resolve back to the face that supplied the glyphs.
        fn draws_han(&self, font: &Font) -> bool {
            self.covers_han(font)
                && self.reloads_face(&font.describe_with_absolute_size())
        }

        /// Checks the drawing lookup against the face that supplied the original glyph indices.
        fn reloads_face(&self, shaped: &FontDescription) -> bool {
            self.context.load_font(shaped).is_some_and(|drawn| {
                drawn.describe_with_absolute_size() == *shaped
            })
        }

        /// Ranks fallback families so multi-weight outline fonts beat regular-only legacy fonts.
        fn candidate_score(&self, family: &str) -> u8 {
            let mut score = 0;
            if self.has_bold(family) {
                score += 4;
            }
            // WenQuanYi Zen Hei ships with `hintnone` and no bold face on Linux distributions.
            if !family.starts_with("WenQuanYi Zen Hei") {
                score += 2;
            }
            if !family.starts_with("WenQuanYi") {
                score += 1;
            }
            score
        }

        /// Returns whether `family` resolves to a genuine bold face for headings and emphasis.
        fn has_bold(&self, family: &str) -> bool {
            let mut description = FontDescription::new();
            description.set_family(family);
            description.set_weight(Weight::Bold);
            description.set_size(14 * pangocairo::pango::SCALE);
            self.context.load_font(&description).is_some_and(|font| {
                let actual = font.describe();
                actual.family().as_deref() == Some(family)
                    && actual.weight() >= Weight::Semibold
            })
        }
    }
}
