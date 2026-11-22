//! Layer 3: object shapes allocate fast-property objects, null prototype included, and keep the
//! shape usable across GC.

use std::sync::Once;

use v8_coroutine_sys as v8;
use v8_coroutine_sys::object_shape::ObjectShape;

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        v8::V8::set_flags_from_command_line(vec![
            "v8_coroutine_sys_object_shape_test".to_string(),
            "--allow-natives-syntax".to_string(),
            "--expose-gc".to_string(),
        ]);
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

fn eval<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str) -> v8::Local<'s, v8::Value> {
    let source = v8::String::new(scope, code).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap()
}

fn run<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str) -> String {
    eval(scope, code).to_rust_string_lossy(scope)
}

fn names<'s>(scope: &v8::PinScope<'s, '_>, names: &[&str]) -> Vec<v8::Local<'s, v8::String>> {
    names.iter().map(|name| v8::String::new(scope, name).unwrap()).collect()
}

fn with_scope(test: impl FnOnce(&mut v8::PinScope<'_, '_>)) {
    init();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    test(scope);
}

/// Publishes `row` as `globalThis.row` so a script can inspect it.
fn publish<'s>(scope: &mut v8::PinScope<'s, '_>, row: v8::Local<'s, v8::Object>) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, "row").unwrap();
    global.set(scope, key.into(), row.into());
}

#[test]
fn null_prototype_row_is_a_fast_object() {
    with_scope(|scope| {
        let shape = ObjectShape::new(scope, None, &names(scope, &["Id", "Name", "Note"])).unwrap();
        assert_eq!(shape.len(), 3);
        let number = v8::Number::new(scope, 42.0);
        let text = v8::String::new(scope, "north").unwrap();
        let values: Vec<v8::Local<v8::Value>> = vec![number.into(), text.into(), v8::null(scope).into()];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        assert_eq!(run(scope, "Object.getPrototypeOf(row) === null"), "true");
        assert_eq!(run(scope, "%HasFastProperties(row)"), "true");
        assert_eq!(run(scope, "Object.keys(row).join(',')"), "Id,Name,Note");
        assert_eq!(run(scope, "JSON.stringify(row)"), r#"{"Id":42,"Name":"north","Note":null}"#);
        assert_eq!(run(scope, "row.Id + row.Name"), "42north");
    });
}

#[test]
fn ordinary_prototype_is_honoured() {
    with_scope(|scope| {
        let prototype = eval(scope, "Object.prototype");
        let shape = ObjectShape::new(scope, Some(prototype), &names(scope, &["changes", "rowid"])).unwrap();
        let values: Vec<v8::Local<v8::Value>> =
            vec![v8::Number::new(scope, 1.0).into(), v8::Number::new(scope, 7.0).into()];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        assert_eq!(run(scope, "Object.getPrototypeOf(row) === Object.prototype"), "true");
        assert_eq!(run(scope, "%HasFastProperties(row)"), "true");
        assert_eq!(run(scope, "row.changes"), "1");
        assert_eq!(run(scope, "row.rowid"), "7");
    });
}

#[test]
fn a_declared_prototype_object_is_used() {
    with_scope(|scope| {
        let prototype = eval(scope, "({ greet() { return 'hi'; } })");
        let shape = ObjectShape::new(scope, Some(prototype), &names(scope, &["a"])).unwrap();
        let values: Vec<v8::Local<v8::Value>> = vec![v8::Number::new(scope, 1.0).into()];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        assert_eq!(run(scope, "row.greet()"), "hi");
        assert_eq!(run(scope, "Object.keys(row).join(',')"), "a");
    });
}

#[test]
fn rows_survive_gc_and_the_shape_stays_usable() {
    with_scope(|scope| {
        let shape = ObjectShape::new(scope, None, &names(scope, &["a", "b"])).unwrap();
        let mut rows: Vec<v8::Local<v8::Value>> = Vec::new();
        for index in 0..64 {
            let values: Vec<v8::Local<v8::Value>> =
                vec![v8::Number::new(scope, f64::from(index)).into(), v8::String::new(scope, "x").unwrap().into()];
            rows.push(shape.new_object(scope, &values).unwrap().into());
        }
        let array = v8::Array::new_with_elements(scope, &rows);
        let context = scope.get_current_context();
        let global = context.global(scope);
        let key = v8::String::new(scope, "rows").unwrap();
        global.set(scope, key.into(), array.into());

        run(scope, "for (let i = 0; i < 2000; i++) ({ pad: 'y'.repeat(32), n: i });");
        run(scope, "gc(); gc()");
        assert_eq!(run(scope, "%HasFastProperties(rows[0])"), "true");
        assert_eq!(run(scope, "Object.getPrototypeOf(rows[0]) === null"), "true");
        assert_eq!(run(scope, "rows.length"), "64");
        assert_eq!(run(scope, "rows[63].a + rows[63].b"), "63x");

        // The same shape still allocates after the GC that saw its map survive.
        let values: Vec<v8::Local<v8::Value>> =
            vec![v8::Number::new(scope, 100.0).into(), v8::String::new(scope, "after").unwrap().into()];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);
        assert_eq!(run(scope, "row.a + row.b"), "100after");
        assert_eq!(run(scope, "%HasFastProperties(row)"), "true");
    });
}

#[test]
fn rows_are_writable_and_deletable_after_creation() {
    with_scope(|scope| {
        let shape = ObjectShape::new(scope, None, &names(scope, &["a", "b", "c"])).unwrap();
        let values: Vec<v8::Local<v8::Value>> = vec![
            v8::Number::new(scope, 1.0).into(),
            v8::Number::new(scope, 2.0).into(),
            v8::Number::new(scope, 3.0).into(),
        ];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        run(scope, "row.a = 'changed'");
        assert_eq!(run(scope, "row.a"), "changed");
        run(scope, "delete row.b");
        assert_eq!(run(scope, "row.b"), "undefined");
        assert_eq!(run(scope, "'b' in row"), "false");
        assert_eq!(run(scope, "Object.keys(row).join(',')"), "a,c");
        assert_eq!(run(scope, "Object.getPrototypeOf(row) === null"), "true");
    });
}

#[test]
fn proto_column_name_is_an_own_property() {
    with_scope(|scope| {
        let shape = ObjectShape::new(scope, None, &names(scope, &["__proto__", "x"])).unwrap();
        let values: Vec<v8::Local<v8::Value>> =
            vec![v8::String::new(scope, "data").unwrap().into(), v8::Number::new(scope, 2.0).into()];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        assert_eq!(run(scope, "Object.getPrototypeOf(row) === null"), "true");
        assert_eq!(run(scope, "Object.prototype.hasOwnProperty.call(row, '__proto__')"), "true");
        assert_eq!(run(scope, "Object.getOwnPropertyDescriptor(row, '__proto__').value"), "data");
        assert_eq!(run(scope, "row.x"), "2");
    });
}

#[test]
fn an_empty_column_name_is_an_own_property() {
    with_scope(|scope| {
        let shape = ObjectShape::new(scope, None, &names(scope, &["", "a"])).unwrap();
        let values: Vec<v8::Local<v8::Value>> =
            vec![v8::Number::new(scope, 1.0).into(), v8::Number::new(scope, 2.0).into()];
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        assert_eq!(run(scope, "Object.keys(row).join('|')"), "|a");
        assert_eq!(run(scope, "row['']"), "1");
    });
}

#[test]
fn the_shape_carries_the_values_it_is_given() {
    with_scope(|scope| {
        let width = 40;
        let owned: Vec<String> = (0..width).map(|index| format!("c{index}")).collect();
        let owned: Vec<&str> = owned.iter().map(String::as_str).collect();
        let shape = ObjectShape::new(scope, None, &names(scope, &owned)).unwrap();
        assert_eq!(shape.len(), width);
        let values: Vec<v8::Local<v8::Value>> =
            (0..width).map(|index| v8::Number::new(scope, index as f64).into()).collect();
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);

        assert_eq!(run(scope, "%HasFastProperties(row)"), "true");
        assert_eq!(run(scope, "row.c0 + row.c39"), "39");
        assert_eq!(run(scope, "Object.keys(row).length"), "40");
    });
}

#[test]
fn a_wrong_value_count_is_refused() {
    with_scope(|scope| {
        let shape = ObjectShape::new(scope, None, &names(scope, &["a", "b"])).unwrap();
        let one: Vec<v8::Local<v8::Value>> = vec![v8::Number::new(scope, 1.0).into()];
        assert!(shape.new_object(scope, &one).is_none());
        let three: Vec<v8::Local<v8::Value>> = vec![
            v8::Number::new(scope, 1.0).into(),
            v8::Number::new(scope, 2.0).into(),
            v8::Number::new(scope, 3.0).into(),
        ];
        assert!(shape.new_object(scope, &three).is_none());
    });
}

#[test]
fn unrepresentable_names_are_refused() {
    with_scope(|scope| {
        assert!(ObjectShape::new(scope, None, &[]).is_none());
        assert!(ObjectShape::new(scope, None, &names(scope, &["0"])).is_none());
        assert!(ObjectShape::new(scope, None, &names(scope, &["a", "12", "b"])).is_none());
        assert!(ObjectShape::new(scope, None, &names(scope, &["a", "a"])).is_none());
        let wide: Vec<String> = (0..128).map(|index| format!("c{index}")).collect();
        let wide: Vec<&str> = wide.iter().map(String::as_str).collect();
        assert!(ObjectShape::new(scope, None, &names(scope, &wide)).is_none());
        // 127 names still fit the map cache and stay in the object body.
        let wide: Vec<String> = (0..127).map(|index| format!("c{index}")).collect();
        let wide: Vec<&str> = wide.iter().map(String::as_str).collect();
        let shape = ObjectShape::new(scope, None, &names(scope, &wide)).unwrap();
        assert_eq!(shape.len(), 127);
        let values: Vec<v8::Local<v8::Value>> =
            (0..127).map(|index| v8::Number::new(scope, index as f64).into()).collect();
        let row = shape.new_object(scope, &values).unwrap();
        publish(scope, row);
        assert_eq!(run(scope, "%HasFastProperties(row)"), "true");
        assert_eq!(run(scope, "row.c126"), "126");
    });
}

/// Throughput of the bulk builder alone, away from SQLite.
///
/// `cargo test --release -p v8-coroutine-sys --test object_shape -- --ignored --nocapture bulk_builder`
#[test]
#[ignore = "measurement: run with --release --ignored --nocapture"]
fn bulk_builder_throughput() {
    with_scope(|scope| {
        let rows = 621_883usize;
        let columns = 6usize;
        let names = names(scope, &["Id", "OrderId", "ProductId", "UnitPrice", "Quantity", "Discount"]);
        let shape = ObjectShape::new(scope, None, &names).unwrap();
        let fields: Vec<u32> = (0..columns as u32).collect();
        let kinds = vec![1u8; rows * columns];
        // A mix like OrderDetail: mostly Smis with a few real numbers.
        let numbers: Vec<f64> =
            (0..rows * columns).map(|index| if index % 9 == 0 { 12.5 } else { (index % 97) as f64 }).collect();
        let values: Vec<v8::Local<v8::Value>> = Vec::new();

        for round in 0..3 {
            let start = std::time::Instant::now();
            let array = shape.new_objects(scope, &fields, &kinds, &numbers, &values, rows).unwrap();
            let elapsed = start.elapsed();
            assert_eq!(array.length(), rows as u32);
            println!("round {round}: {elapsed:?}, {:.1} ns/row", elapsed.as_nanos() as f64 / rows as f64);
        }
    });
}
