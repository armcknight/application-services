/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::db::StorageConn;
use crate::error::*;
use serde_derive::Serialize;
use serde_json::{Map, Value as JsonValue};
use sql_support::{self, ConnExt};
use std::collections::HashMap;

const QUOTA_BYTES: usize = 102_400;
const QUOTA_BYTES_PER_ITEM: usize = 8_192;
const MAX_ITEMS: usize = 512;
// Note there are constants for "operations per minute" etc, which aren't
// enforced here.

type JsonMap = Map<String, JsonValue>;

fn get_from_db(conn: &StorageConn, ext_guid: &str) -> Result<Option<JsonMap>> {
    Ok(
        match conn.try_query_one::<String>(
            "SELECT data FROM moz_extension_data
             WHERE guid = :guid",
            &[(":guid", &ext_guid)],
            true,
        )? {
            Some(s) => match serde_json::from_str(&s)? {
                JsonValue::Object(m) => Some(m),
                // we could panic here as it's theoretically impossible, but we
                // might as well treat it as not existing...
                _ => None,
            },
            None => None,
        },
    )
}

fn save_to_db(conn: &StorageConn, ext_guid: &str, val: &JsonValue) -> Result<()> {
    // Convert to bytes so we can enforce the quota.
    let sval = val.to_string();
    let bytes: Vec<u8> = sval.bytes().collect();
    if bytes.len() > QUOTA_BYTES {
        return Err(ErrorKind::QuotaError(QuotaReason::TotalBytes).into());
    }
    // XXX - work out how to get use these bytes directly instead of sval, so
    // we don't utf-8 encode twice!

    // XXX - sync support will need to do the syncStatus thing here.
    conn.execute_named(
        "INSERT OR REPLACE INTO moz_extension_data(guid, data)
            VALUES (:guid, :data)",
        &[(":guid", &ext_guid), (":data", &sval)],
    )?;
    Ok(())
}

fn remove_from_db(conn: &StorageConn, ext_guid: &str) -> Result<()> {
    // XXX - sync support will need to do the tombstone thing here.
    conn.execute_named(
        "DELETE FROM moz_extension_data
        WHERE guid = :guid",
        &[(":guid", &ext_guid)],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageValueChange {
    #[serde(skip_serializing_if = "Option::is_none")]
    old_value: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_value: Option<JsonValue>,
}

pub type StorageChanges = HashMap<String, StorageValueChange>;

pub fn set(conn: &StorageConn, ext_guid: &str, val: JsonValue) -> Result<StorageChanges> {
    // XXX - Should we consider making this function  take a &str, and parse
    // it ourselves? That way we could avoid parsing entirely if no existing
    // value (but presumably that's going to be the uncommon case, so it probably
    // doesn't matter)
    let val_map = match val {
        JsonValue::Object(m) => m,
        // Not clear what the error semantics should be yet. For now, pretend an empty map.
        _ => Map::new(),
    };

    let mut current = get_from_db(conn, ext_guid)?.unwrap_or_else(Map::new);

    let mut changes = StorageChanges::with_capacity(val_map.len());

    // iterate over the value we are adding/updating.
    for (k, v) in val_map.into_iter() {
        let old_value = current.remove(&k);
        if current.len() >= MAX_ITEMS {
            return Err(ErrorKind::QuotaError(QuotaReason::MaxItems).into());
        }
        // Sadly we need to stringify the value here just to check the quota.
        // Reading the chrome docs literally, the length of the key is just
        // the string len, but the value is the json val.
        if k.bytes().count() + v.to_string().bytes().count() >= QUOTA_BYTES_PER_ITEM {
            return Err(ErrorKind::QuotaError(QuotaReason::ItemBytes).into());
        }
        current.insert(k.clone(), v.clone());
        let change = StorageValueChange {
            old_value,
            new_value: Some(v),
        };
        changes.insert(k, change);
    }

    save_to_db(conn, ext_guid, &JsonValue::Object(current))?;
    Ok(changes)
}

// A helper which takes a param indicating what keys should be returned and
// converts that to a vec of real strings. Also returns "default" values to
// be used if no item exists for that key.
fn get_keys(keys: &JsonValue) -> Vec<(String, Option<JsonValue>)> {
    match keys {
        JsonValue::String(s) => vec![(s.to_string(), None)],
        JsonValue::Array(keys) => {
            // because nothing with json is ever simple, each key may not be
            // a string. We ignore any which aren't.
            keys.iter()
                .filter_map(|v| v.as_str().map(|s| (s.to_string(), None)))
                .collect()
        }
        // XXX - we clone the map value here, but `remove()` doesn't need it - maybe
        // we should take a param to indicate if the defaults are actually needed?
        // (Or maybe lifetimes magic could make the clone unnecessary? It should have
        // the same lifetime as `keys`)
        JsonValue::Object(m) => m
            .iter()
            .map(|(k, d)| (k.to_string(), Some(d.clone())))
            .collect(),
        _ => vec![],
    }
}

// XXX - is this signature OK? We never return None, only Null
pub fn get(conn: &StorageConn, ext_guid: &str, keys: &JsonValue) -> Result<JsonValue> {
    // key is optional, or string or array of string or object keys
    let maybe_existing = get_from_db(conn, ext_guid)?;
    let mut existing = match maybe_existing {
        None => return Ok(JsonValue::Object(Map::new())),
        Some(v) => v,
    };
    // take the quick path for null, where we just return the entire object.
    if keys == &JsonValue::Null {
        return Ok(JsonValue::Object(existing));
    }
    // OK, so we need to build a list of keys to get.
    let keys_and_defaults = get_keys(keys);
    let mut result = Map::with_capacity(keys_and_defaults.len());
    for (key, maybe_default) in keys_and_defaults {
        // XXX - assume that if key doesn't exist, it doesn't exist in the result.
        if let Some(v) = existing.remove(&key) {
            result.insert(key.to_string(), v);
        } else if let Some(def) = maybe_default {
            result.insert(key.to_string(), def);
        }
    }
    Ok(JsonValue::Object(result))
}

pub fn remove(conn: &StorageConn, ext_guid: &str, keys: &JsonValue) -> Result<StorageChanges> {
    let mut existing = match get_from_db(conn, ext_guid)? {
        None => return Ok(StorageChanges::new()),
        Some(v) => v,
    };

    let keys_and_defs = get_keys(keys);

    let mut result = StorageChanges::with_capacity(keys_and_defs.len());
    for (key, _) in keys_and_defs {
        if let Some(v) = existing.remove(&key) {
            result.insert(
                key.to_string(),
                StorageValueChange {
                    old_value: Some(v),
                    new_value: None,
                },
            );
        }
    }
    if !result.is_empty() {
        save_to_db(conn, ext_guid, &JsonValue::Object(existing))?;
    }
    Ok(result)
}

pub fn clear(conn: &StorageConn, ext_guid: &str) -> Result<StorageChanges> {
    // XXX - transaction?
    let existing = match get_from_db(conn, ext_guid)? {
        None => return Ok(StorageChanges::new()),
        Some(v) => v,
    };
    let mut result = StorageChanges::with_capacity(existing.len());
    for (key, val) in existing.into_iter() {
        result.insert(
            key.to_string(),
            StorageValueChange {
                new_value: None,
                old_value: Some(val),
            },
        );
    }
    remove_from_db(conn, ext_guid)?;
    Ok(result)
}

// XXX - get_bytes_in_use()

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test::new_mem_connection;
    use serde_json::json;

    fn make_changes(changes: &[(&str, Option<JsonValue>, Option<JsonValue>)]) -> StorageChanges {
        let mut r = StorageChanges::with_capacity(changes.len());
        for (name, old_value, new_value) in changes {
            r.insert(
                (*name).to_string(),
                StorageValueChange {
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                },
            );
        }
        r
    }

    #[test]
    fn test_simple() -> Result<()> {
        let ext_id = "x";
        let conn = new_mem_connection();

        // an empty store.
        for q in &[
            &JsonValue::Null,
            &json!("foo"),
            &json!(["foo"]),
            &json!({ "foo": null }),
            &json!({"foo": "default"}),
        ] {
            assert_eq!(get(&conn, &ext_id, q)?, json!({}));
        }

        // Single item in the store.
        set(&conn, &ext_id, json!({"foo": "bar" }))?;
        for q in &[
            &JsonValue::Null,
            &json!("foo"),
            &json!(["foo"]),
            &json!({ "foo": null }),
            &json!({"foo": "default"}),
        ] {
            assert_eq!(get(&conn, &ext_id, q)?, json!({"foo": "bar" }));
        }

        // more complex stuff, including changes checking.
        assert_eq!(
            set(&conn, &ext_id, json!({"foo": "new", "other": "also new" }))?,
            make_changes(&[
                ("foo", Some(json!("bar")), Some(json!("new"))),
                ("other", None, Some(json!("also new")))
            ])
        );
        assert_eq!(
            get(&conn, &ext_id, &JsonValue::Null)?,
            json!({"foo": "new", "other": "also new"})
        );
        assert_eq!(get(&conn, &ext_id, &json!("foo"))?, json!({"foo": "new"}));
        assert_eq!(
            get(&conn, &ext_id, &json!(["foo", "other"]))?,
            json!({"foo": "new", "other": "also new"})
        );
        assert_eq!(
            get(&conn, &ext_id, &json!({"foo": null, "default": "yo"}))?,
            json!({"foo": "new", "default": "yo"})
        );

        assert_eq!(
            remove(&conn, &ext_id, &json!("foo"))?,
            make_changes(&[("foo", Some(json!("new")), None)]),
        );
        // XXX - other variants.

        assert_eq!(
            clear(&conn, &ext_id)?,
            make_changes(&[("other", Some(json!("also new")), None)]),
        );
        assert_eq!(get(&conn, &ext_id, &JsonValue::Null)?, json!({}));

        Ok(())
    }

    #[test]
    fn test_check_get_impl() -> Result<()> {
        // This is a port of checkGetImpl in test_ext_storage.js in Desktop.
        let ext_id = "x";
        let conn = new_mem_connection();

        let prop = "test-prop";
        let value = "test-value";

        set(&conn, ext_id, json!({ prop: value }))?;

        // this is the checkGetImpl part!
        let mut data = get(&conn, &ext_id, &json!(null))?;
        assert_eq!(value, json!(data[prop]), "null getter worked for {}", prop);

        data = get(&conn, &ext_id, &json!(prop))?;
        assert_eq!(
            value,
            json!(data[prop]),
            "string getter worked for {}",
            prop
        );
        assert_eq!(
            data.as_object().unwrap().len(),
            1,
            "string getter should return an object with a single property"
        );

        data = get(&conn, &ext_id, &json!([prop]))?;
        assert_eq!(value, json!(data[prop]), "array getter worked for {}", prop);
        assert_eq!(
            data.as_object().unwrap().len(),
            1,
            "array getter with a single key should return an object with a single property"
        );

        // checkGetImpl() uses `{ [prop]: undefined }` - but json!() can't do that :(
        // Hopefully it's just testing a simple object, so we use `{ prop: null }`
        data = get(&conn, &ext_id, &json!({ prop: null }))?;
        assert_eq!(
            value,
            json!(data[prop]),
            "object getter worked for {}",
            prop
        );
        assert_eq!(
            data.as_object().unwrap().len(),
            1,
            "object getter with a single key should return an object with a single property"
        );

        Ok(())
    }

    #[test]
    fn test_bug_1621162() -> Result<()> {
        // apparently Firefox, unlike Chrome, will not optimize the changes.
        // See bug 1621162 for more!
        let conn = new_mem_connection();
        let ext_id = "xyz";

        set(&conn, &ext_id, json!({"foo": "bar" }))?;

        assert_eq!(
            set(&conn, &ext_id, json!({"foo": "bar" }))?,
            make_changes(&[("foo", Some(json!("bar")), Some(json!("bar")))]),
        );
        Ok(())
    }

    #[test]
    fn test_quota_maxitems() -> Result<()> {
        let conn = new_mem_connection();
        let ext_id = "xyz";
        for i in 1..MAX_ITEMS + 1 {
            set(
                &conn,
                &ext_id,
                json!({ format!("key-{}", i): format!("value-{}", i) }),
            )?;
        }
        let e = set(&conn, &ext_id, json!({"another": "another"})).unwrap_err();
        match e.kind() {
            ErrorKind::QuotaError(QuotaReason::MaxItems) => {}
            _ => panic!("unexpected error type"),
        };
        Ok(())
    }

    #[test]
    fn test_quota_bytesperitem() -> Result<()> {
        let conn = new_mem_connection();
        let ext_id = "xyz";
        // A string 5 bytes less than the max. This should be counted as being
        // 3 bytes less than the max as the quotes are counted.
        let val = "x".repeat(QUOTA_BYTES_PER_ITEM - 5);

        // Key length doesn't push it over.
        set(&conn, &ext_id, json!({ "x": val }))?;

        // Key length does push it over.
        let e = set(&conn, &ext_id, json!({ "xxxx": val })).unwrap_err();
        match e.kind() {
            ErrorKind::QuotaError(QuotaReason::ItemBytes) => {}
            _ => panic!("unexpected error type"),
        };
        Ok(())
    }
}
