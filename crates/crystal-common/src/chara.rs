//! The character data the lobby carries as JSON text: `CharaMake` (what the client sends when a
//! character is made) and `ClientSelectData` (what character select shows, in each list entry).
//!
//! Both are the client's `WorkDefinitions` classes, written as
//! `{"classid":N,"classname":"...","content":[...]}` with one `content` element per field, in field
//! order: a string for a scalar or a char array, an array of strings for a numeric array. The field
//! types are the client's (each class's `_TypeInfo` slot 18: `CharaMake` @0xe5ea60,
//! `ClientSelectData` @0xf40d10); class ids from slot 1 (0x76 @0xe5b770, 0x74 @0xf35b70), field
//! counts from slot 5 (7 @0xe67be0, 23 @0xf63500).

use serde_json::Value;

/// What the client sends in step 2 of making a character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharaMake {
    /// The 26 appearance values (race, sex, ..., the same field `ClientSelectData` carries).
    pub customize: [u8; 26],
    pub voice: u8,
    pub guardian: u8,
    pub birth_month: u8,
    pub birth_day: u8,
    /// The starting class (`ClassJob` row).
    pub class: u8,
    /// The starting city (1 Limsa Lominsa, 2 Gridania, 3 Ul'dah).
    pub city: u8,
}

pub const CHARA_MAKE_CLASS_ID: u32 = 0x76;
pub const CLIENT_SELECT_DATA_CLASS_ID: u32 = 0x74;

fn num(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

impl CharaMake {
    /// Parses the client's text; `None` when it is not a `CharaMake` with 7 fields.
    pub fn parse(json: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(json).ok()?;
        if v.get("classid").and_then(num) != Some(CHARA_MAKE_CLASS_ID as u64) {
            return None;
        }
        let content = v.get("content")?.as_array()?;
        if content.len() != 7 {
            return None;
        }
        let mut customize = [0u8; 26];
        let values = content[0].as_array()?;
        for (slot, value) in customize.iter_mut().zip(values) {
            *slot = num(value)? as u8;
        }
        let byte = |i: usize| num(&content[i]).map(|n| n as u8);
        Some(Self {
            customize,
            voice: byte(1)?,
            guardian: byte(2)?,
            birth_month: byte(3)?,
            birth_day: byte(4)?,
            class: byte(5)?,
            city: byte(6)?,
        })
    }
}

/// What character select shows for a character: `ClientSelectData` (23 fields). Fields whose meaning
/// is not known yet are sent as zeros; the order and types are the client's, the meanings of fields
/// 5 to 10 are those later ARR servers use (birth month, day, guardian, class, -, zone).
#[derive(Debug, Clone, Default)]
pub struct ClientSelectData {
    pub name: String,
    /// Level per class, 19 of them in this build.
    pub levels: [u16; 19],
    pub birth_month: u8,
    pub birth_day: u8,
    pub guardian: u8,
    pub class: u8,
    pub zone_id: u16,
    pub customize: [u8; 26],
    pub main_weapon: u64,
    pub sub_weapon: u64,
    /// Equipment models, 10 slots.
    pub models: [u32; 10],
}

impl ClientSelectData {
    pub fn from_chara_make(name: &str, make: &CharaMake, zone_id: u16) -> Self {
        Self {
            name: name.to_owned(),
            levels: [1; 19],
            birth_month: make.birth_month,
            birth_day: make.birth_day,
            guardian: make.guardian,
            class: make.class,
            zone_id,
            customize: make.customize,
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> String {
        let s = |v: &dyn ToString| Value::String(v.to_string());
        let list = |vs: Vec<String>| Value::Array(vs.into_iter().map(Value::String).collect());
        let content = vec![
            Value::String(self.name.clone()),                             // 0  char[32]
            list(self.levels.iter().map(|l| l.to_string()).collect()),    // 1  u16[19]
            s(&0),                                                        // 2  u8
            s(&0),                                                        // 3  u8
            s(&0),                                                        // 4  u8
            s(&self.birth_month),                                         // 5  u8
            s(&self.birth_day),                                           // 6  u8
            s(&self.guardian),                                            // 7  u8
            s(&self.class),                                               // 8  u8
            s(&0),                                                        // 9  u16
            s(&self.zone_id),                                             // 10 u16
            list(self.customize.iter().map(|c| c.to_string()).collect()), // 11 u8[26]
            s(&self.main_weapon),                                         // 12 u64
            s(&self.sub_weapon),                                          // 13 u64
            list(self.models.iter().map(|m| m.to_string()).collect()),    // 14 u32[10]
            s(&0),                                                        // 15 u32
            s(&0),                                                        // 16 u32
            s(&0),                                                        // 17 u32
            s(&0),                                                        // 18 u32
            s(&0),                                                        // 19 u16
            s(&0),                                                        // 20 u8
            Value::String(String::new()),                                 // 21 char[32]
            s(&0),                                                        // 22 u64
        ];
        serde_json::json!({
            "classid": CLIENT_SELECT_DATA_CLASS_ID,
            "classname": "ClientSelectData",
            "content": content,
        })
        .to_string()
    }
}

/// Where a new character first stands, by its starting city: a city territory and a point in it
/// (x, y, z, rotation). Not the opening territories (181-183): those are private areas whose
/// opening event the client waits for before it shows the world. The points stand beside event
/// NPCs placed in the client's `Level` sheet (rows 4069046 in 129 Limsa Lominsa Lower Decks,
/// 1140471 in 132 New Gridania, 2360563 in 130 Ul'dah - Steps of Nald), so on walkable ground.
pub fn start_point(city: u8) -> (u16, [f64; 4]) {
    match city {
        2 => (132, [27.0, -8.0, 108.0, 1.6]),
        3 => (130, [-86.0, 2.55, -51.0, 1.5]),
        _ => (129, [-62.0, 18.0, -4.0, 0.2]),
    }
}

pub fn starting_zone(city: u8) -> u16 {
    start_point(city).0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the client sent it (the session's log).
    const MADE: &str = r#"{"classid":118,"classname":"CharaMake","content":[["1","0","1","50","2","3","3","0","28","54","55","0","31","2","2","54","0","0","0","0","43","50","0","0","0","36"],"52","9","3","21","3","1"]}"#;

    #[test]
    fn chara_make_from_client() {
        let m = CharaMake::parse(MADE).unwrap();
        assert_eq!(&m.customize[..5], &[1, 0, 1, 50, 2]);
        assert_eq!(m.customize[25], 36);
        assert_eq!((m.voice, m.guardian, m.birth_month, m.birth_day, m.class, m.city), (52, 9, 3, 21, 3, 1));
        assert_eq!(CharaMake::parse(r#"{"classid":116,"content":[]}"#), None);
    }

    #[test]
    fn client_select_data_shape() {
        let m = CharaMake::parse(MADE).unwrap();
        let json = ClientSelectData::from_chara_make("Null Crystal", &m, starting_zone(m.city)).to_json();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["classid"], 116);
        assert_eq!(v["classname"], "ClientSelectData");
        let c = v["content"].as_array().unwrap();
        assert_eq!(c.len(), 23);
        assert_eq!(c[0], "Null Crystal");
        assert_eq!(c[1].as_array().unwrap().len(), 19);
        assert_eq!((c[8].as_str(), c[10].as_str()), (Some("3"), Some("129")));
        assert_eq!(c[11].as_array().unwrap().len(), 26);
        assert_eq!(c[14].as_array().unwrap().len(), 10);
        assert!(json.len() < 0x450 - 0x4a, "fits the entry's text field");
    }
}
