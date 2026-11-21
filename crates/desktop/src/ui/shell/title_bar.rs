//! Title bar: sidebar toggle, the in-window menu bar off macOS, and the window title.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, TitleBar};
use gpui_kit::{
    Context, IntoElement, ParentElement as _, Styled as _, div,
    prelude::FluentBuilder as _,
};

use crate::actions::ToggleSidebar;
use crate::ui::AppView;

impl AppView {
    /// Renders the title bar on the chrome surface, without its own border.
    pub(in crate::ui) fn render_title_bar(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let chrome = cx.theme().title_bar;
        let muted = cx.theme().muted_foreground;
        // The title bar spreads its children apart, so the leading controls form one group
        // and the title takes the middle.
        let leading = div()
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .child(
                Button::new("toggle-sidebar")
                    .ghost()
                    .small()
                    .icon(IconName::PanelLeft)
                    .tooltip_with_action(
                        rust_i18n::t!("Sidebar.toggle").to_string(),
                        &ToggleSidebar,
                        Some(crate::actions::WORKSPACE_CONTEXT),
                    )
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleSidebar), cx)
                    }),
            )
            // macOS shows the same menus in the system menu bar.
            .when(!cfg!(target_os = "macos"), |this| {
                this.child(self.menu_bar.clone())
            });
        TitleBar::new()
            .bg(chrome)
            .border_b_0()
            .child(leading)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .text_sm()
                    .text_color(muted)
                    .child(rust_i18n::t!("App.title").to_string()),
            )
    }
}

/// Publishes the application menus to the platform and the in-window menu bar, in the current language.
pub(in crate::ui) fn install_menus(
    menu_bar: &gpui_kit::Entity<gpui_kit::component::menu::AppMenuBar>,
    cx: &mut gpui_kit::App,
) {
    use gpui_kit::{Menu, MenuItem};

    use crate::actions::{CloseTab, NewSession, OpenSettings, Quit};

    let build = || {
        vec![
            Menu::new(rust_i18n::t!("Menu.file").into_owned()).items([
                MenuItem::action(
                    rust_i18n::t!("Sidebar.new_session").into_owned(),
                    NewSession,
                ),
                MenuItem::action(
                    rust_i18n::t!("Menu.close_tab").into_owned(),
                    CloseTab,
                ),
                MenuItem::action(
                    rust_i18n::t!("Menu.settings").into_owned(),
                    OpenSettings,
                ),
                MenuItem::separator(),
                MenuItem::action(rust_i18n::t!("Menu.quit").into_owned(), Quit),
            ]),
            Menu::new(rust_i18n::t!("Menu.view").into_owned()).items([
                MenuItem::action(
                    rust_i18n::t!("Sidebar.toggle").into_owned(),
                    ToggleSidebar,
                ),
            ]),
        ]
    };
    cx.set_menus(build());
    gpui_kit::component::GlobalState::global_mut(cx)
        .set_app_menus(build().into_iter().map(Menu::owned).collect());
    menu_bar.update(cx, |menu_bar, cx| menu_bar.reload(cx));
}
