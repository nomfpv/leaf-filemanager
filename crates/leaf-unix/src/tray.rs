//! Linux backend of rf_win::tray. Status: REAL (written blind, not tried on a desktop): a StatusNotifierItem on the session bus
//! (`org.kde.StatusNotifierItem`) with its menu as `com.canonical.dbusmenu`, over zbus on the shared session bus. A click opens leaf;
//! the menu has Open, New window, Settings and Exit. With no StatusNotifierWatcher on the bus (GNOME without the extension, no bus)
//! there is no icon and [`show`] only logs it.

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{Array, ObjectPath, OwnedValue, StructureBuilder, Type, Value};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

static HANDLER: Mutex<Option<Box<dyn Fn(TrayEvent) + Send>>> = Mutex::new(None);
static LABELS: Mutex<Option<Labels>> = Mutex::new(None);
static NAME: Mutex<Option<String>> = Mutex::new(None);

/// Calls `f` for what the user picks from the icon or its menu (on a thread of the bus).
pub fn set_handler(f: impl Fn(TrayEvent) + Send + 'static) {
    *HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

fn fire(e: TrayEvent) {
    if let Some(h) = HANDLER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        h(e);
    }
}

// TYPE-COPY(rf-win src/tray.rs:29)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    NewWindow,
    Settings,
    Quit,
}

// TYPE-COPY(rf-win src/tray.rs:40)
#[derive(Clone)]
pub struct Labels {
    pub tooltip: String,
    pub open: String,
    pub new_window: String,
    pub settings: String,
    pub quit: String,
}


fn labels() -> Labels {
    LABELS.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or(Labels { tooltip: "leaf".into(), open: "Open leaf".into(), new_window: "New window".into(), settings: "Settings".into(), quit: "Exit".into() })
}

struct Item;

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {
        fire(TrayEvent::Open);
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        fire(TrayEvent::NewWindow);
    }
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".into()
    }
    #[zbus(property)]
    fn id(&self) -> String {
        crate::APP_EXE_STEM.into()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        labels().tooltip
    }
    #[zbus(property)]
    fn status(&self) -> String {
        "Active".into()
    }
    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        crate::APP_EXE_STEM.into()
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        ObjectPath::from_static_str_unchecked(MENU_PATH)
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        (String::new(), Vec::new(), labels().tooltip, String::new())
    }
}

/// `(ia{sv}av)`: a menu entry with its children (each child a variant holding another of these).
#[derive(Type, serde::Serialize)]
struct Layout(i32, HashMap<String, Value<'static>>, Vec<Value<'static>>);

const IDS: [(i32, TrayEvent); 4] = [(1, TrayEvent::Open), (2, TrayEvent::NewWindow), (3, TrayEvent::Settings), (4, TrayEvent::Quit)];

fn text_of(id: i32) -> String {
    let l = labels();
    match id {
        1 => l.open,
        2 => l.new_window,
        3 => l.settings,
        _ => l.quit,
    }
}

fn props_of(id: i32) -> HashMap<String, Value<'static>> {
    let mut m = HashMap::new();
    if id == 0 {
        m.insert("children-display".to_string(), Value::from("submenu"));
    } else {
        m.insert("label".to_string(), Value::from(text_of(id)));
    }
    m
}

/// A leaf of the menu as the variant the layout carries: `(ia{sv}av)` with no children.
fn child(id: i32) -> Option<Value<'static>> {
    let none: Vec<Value<'static>> = Vec::new();
    StructureBuilder::new().add_field(id).add_field(props_of(id)).add_field(Array::from(none)).build().ok().map(Value::Structure)
}

fn owned(m: HashMap<String, Value<'static>>) -> HashMap<String, OwnedValue> {
    m.into_iter().filter_map(|(k, v)| OwnedValue::try_from(v).ok().map(|v| (k, v))).collect()
}

struct Menu;

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, parent: i32, _depth: i32, _names: Vec<String>) -> (u32, Layout) {
        let kids = if parent == 0 {
            IDS.iter().filter_map(|(id, _)| child(*id)).collect()
        } else {
            Vec::new()
        };
        (1, Layout(parent, props_of(parent), kids))
    }
    fn get_group_properties(&self, ids: Vec<i32>, _names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        ids.into_iter().map(|i| (i, owned(props_of(i)))).collect()
    }
    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedValue> {
        props_of(id).remove(name).and_then(|v| OwnedValue::try_from(v).ok()).ok_or_else(|| zbus::fdo::Error::InvalidArgs("no such property".into()))
    }
    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        if event_id == "clicked" {
            if let Some((_, e)) = IDS.iter().find(|(i, _)| *i == id) {
                fire(*e);
            }
        }
    }
    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, ev, _, _) in events {
            if ev == "clicked" {
                if let Some((_, e)) = IDS.iter().find(|(i, _)| *i == id) {
                    fire(*e);
                }
            }
        }
        Vec::new()
    }
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }
    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".into()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        "normal".into()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(signal)]
    async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
}

/// Puts the icon in the notification area (when something there listens for it). Quiet when there is none.
pub fn show(labels: Labels) {
    *LABELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(labels);
    if NAME.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return;
    }
    let Some(bus) = crate::bus::session() else { return };
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let r: zbus::Result<()> = (|| {
        let os = bus.object_server();
        os.at(ITEM_PATH, Item)?;
        os.at(MENU_PATH, Menu)?;
        bus.request_name(name.as_str())?;
        bus.call_method(Some("org.kde.StatusNotifierWatcher"), "/StatusNotifierWatcher", Some("org.kde.StatusNotifierWatcher"), "RegisterStatusNotifierItem", &(name.as_str(),))?;
        Ok(())
    })();
    match r {
        Ok(()) => *NAME.lock().unwrap_or_else(|e| e.into_inner()) = Some(name),
        Err(e) => {
            log::info!("tray: no status notifier host ({e})");
            let os = bus.object_server();
            let _ = os.remove::<Item, _>(ITEM_PATH);
            let _ = os.remove::<Menu, _>(MENU_PATH);
            let _ = bus.release_name(name.as_str());
        }
    }
}

pub fn hide() {
    let Some(name) = NAME.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if let Some(bus) = crate::bus::session() {
        let os = bus.object_server();
        let _ = os.remove::<Item, _>(ITEM_PATH);
        let _ = os.remove::<Menu, _>(MENU_PATH);
        let _ = bus.release_name(name.as_str());
    }
}

/// A balloon from the icon: a desktop notification.
pub fn notify(title: &str, body: &str) {
    let toast = crate::toast_post::Toast { tag: "tray", title, lines: &[body], launch: "", buttons: &[], stays: false };
    if let Err(e) = crate::toast_post::show(crate::APP_EXE_STEM, &toast, |_| {}) {
        log::info!("tray: no notification ({e})");
    }
}
