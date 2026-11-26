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

    use pangocairo::pango::{
        Context, CoverageLevel, Font, FontDescription, Language,
    };
    use pangocairo::prelude::*;

    /// Mirrors NSH's font selection on the engine thread and face lookup on the drawing thread.
    pub(super) struct FontFallback {
        context: Context,
        language: Language,
    }

    impl FontFallback {
        /// Probes the two font roles independently so healthy system defaults stay in use.
        pub(super) fn detect() -> BTreeMap<String, String> {
            let probe = Self {
                context: pangocairo::FontMap::new().create_context(),
                language: Language::default(),
            };
            [("font-sans", "sans-serif"), ("font-mono", "monospace")]
                .into_iter()
                .filter_map(|(token, generic)| {
                    probe
                        .replacement(generic)
                        .map(|family| (token.to_owned(), family))
                })
                .collect()
        }

        /// Finds an installed fallback whose shaped glyph indices remain valid when NSH draws them.
        fn replacement(&self, generic: &str) -> Option<String> {
            let mut description = FontDescription::new();
            description.set_family(generic);
            description.set_size(14 * pangocairo::pango::SCALE);
            let fonts =
                self.context.load_fontset(&description, &self.language)?;
            let shaped = fonts.font(u32::from('中'));
            if self.draws_han(&shaped) {
                return None;
            }
            // Fontconfig orders this set by the system's preferences for this role.
            let mut replacement = None;
            fonts.foreach(|_, candidate| {
                if self.draws_han(candidate) {
                    replacement = candidate
                        .describe()
                        .family()
                        .map(|name| name.to_string());
                }
                replacement.is_some()
            });
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

        /// A fallback must cover Chinese and resolve back to the face that supplied the glyphs.
        fn draws_han(&self, font: &Font) -> bool {
            let description = font.describe_with_absolute_size();
            let coverage = font.coverage(&self.language);
            ['中', '文', '。']
                .into_iter()
                .all(|c| coverage.get(c as i32) == CoverageLevel::Exact)
                && self.context.load_font(&description).is_some_and(|drawn| {
                    drawn.describe_with_absolute_size() == description
                })
        }
    }
}
