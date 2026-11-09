//! The connection dialog's choices, swatches, placeholders and Port width.
use crate::support;
use dbento_app::connection_dialog::{ConnectionForm, Field, placeholder};
use dbento_app::ui::segment_colors;
use dbento_core::{Engine, connection_form};
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, Hsla, TestAppContext, px};

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ConnectionForm>) {
    support::init(cx);
    cx.update(|cx| {
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| ConnectionForm::new(None, Vec::new(), false, window, cx))
        })
    })
    .unwrap()
}
fn contrast(a: Hsla, b: Hsla) -> f32 {
    let lum = |c: Hsla| {
        let c = c.to_rgb();
        let ch = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[gpui_kit::test]
fn the_selected_segment_reads_on_its_fill_and_stands_out_from_the_dialog(cx: &mut TestAppContext) {
    support::init(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            let t = cx.theme().clone();
            let (on, off) = (segment_colors(true, cx), segment_colors(false, cx));
            assert_eq!(
                (on.text, on.fill),
                (t.foreground, t.list_active),
                "{mode:?}"
            );
            assert!(contrast(on.text, on.fill) >= 4.5, "{mode:?} text");
            assert!(contrast(on.border, t.popover) >= 3.0, "{mode:?} border");
            assert_eq!(off.text, t.muted_foreground, "{mode:?}");
            assert_ne!(on.fill, off.fill, "{mode:?}");
        });
    }
}

#[gpui_kit::test]
fn a_click_moves_the_selection(cx: &mut TestAppContext) {
    let (win, _form) = open(cx);
    cx.update_window(win, |_, w, cx| w.within("conn-tls").click(1usize, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let tls = w.within("conn-tls");
        assert_eq!(tls.find(1usize).selected(), Some(true));
        assert_eq!(tls.find(0usize).selected(), Some(false));
        assert_eq!(tls.find(2usize).label(), Some("Off"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn swatches_are_14_px_marks_in_20_px_slots_and_port_is_narrow(cx: &mut TestAppContext) {
    let (win, _form) = open(cx);
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        for ix in 0..3usize {
            let slot = w.find(("conn-color", ix)).bounds().size;
            let mark = w.find(("conn-color-mark", ix)).bounds().size;
            assert_eq!((slot.width, slot.height), (px(20.), px(20.)), "slot {ix}");
            assert_eq!((mark.width, mark.height), (px(14.), px(14.)), "mark {ix}");
        }
        assert_eq!(w.find("conn-color-none-mark").bounds().size.width, px(14.));
        let port = w.find("conn-port").bounds().size.width;
        let host = w.find("conn-host").bounds().size.width;
        assert!((f32::from(port) - 96.).abs() <= 1., "{port:?}");
        assert!(host > port * 2., "{host:?}");
    })
    .unwrap();
}

#[test]
fn fields_suggest_their_format() {
    assert_eq!(placeholder(Field::Host), Some("e.g. localhost"));
    assert_eq!(placeholder(Field::Port), Some("5432"));
    assert_eq!(placeholder(Field::Database), Some("e.g. postgres"));
    assert_eq!(placeholder(Field::User), Some("e.g. postgres"));
    assert_eq!(placeholder(Field::Path), Some("path/to/database.sqlite"));
    assert_eq!(placeholder(Field::Timeout), Some("e.g. 30s"));
    assert_eq!(placeholder(Field::Folder), Some("e.g. Production"));
    assert_eq!(placeholder(Field::Password), None);
}

#[test]
fn the_descriptors_carry_the_placeholders_the_form_starts_with() {
    for (engine, field, key) in [
        (Engine::Postgres, Field::Host, "host"),
        (Engine::Postgres, Field::Port, "port"),
        (Engine::Postgres, Field::Database, "database"),
        (Engine::Postgres, Field::User, "user"),
        (Engine::Sqlite, Field::Path, "path"),
    ] {
        let f = connection_form(engine)
            .sections
            .into_iter()
            .flat_map(|s| s.fields)
            .find(|f| f.key == key)
            .unwrap();
        assert_eq!(Some(f.placeholder), placeholder(field), "{key}");
    }
}

#[gpui_kit::test]
fn the_inputs_show_their_placeholders(cx: &mut TestAppContext) {
    let (_win, form) = open(cx);
    for field in [
        Field::Name,
        Field::Path,
        Field::Host,
        Field::Port,
        Field::Database,
        Field::User,
        Field::EnvVar,
        Field::Timeout,
        Field::Folder,
    ] {
        let shown = form.read_with(cx, |f, cx| {
            f.input(field)
                .read(cx)
                .presentation()
                .placeholder()
                .to_string()
        });
        assert_eq!(Some(shown.as_str()), placeholder(field), "{field:?}");
    }
}
