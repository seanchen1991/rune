macro_rules! test_op {
    ($ty:ty => $lhs:literal $op:tt $rhs:literal = $result:literal) => {{
        let program = format!(
            r#"const A = {lhs}; const B = {rhs}; const VALUE = A {op} B; fn main() {{ VALUE }}"#,
            lhs = $lhs, rhs = $rhs, op = stringify!($op),
        );

        assert_eq!(
            $result,
            rune!($ty => &program),
            concat!("expected ", stringify!($result), " out of program `{}`"),
            program
        );
    }}
}

#[test]
fn test_const_values() {
    assert_eq!(
        true,
        rune!(bool => r#"const VALUE = true; fn main() { VALUE }"#)
    );

    assert_eq!(
        "Hello World",
        rune!(String => r#"const VALUE = "Hello World"; fn main() { VALUE }"#)
    );

    assert_eq!(
        "Hello World 1 1.0 true",
        rune!(String => r#"
            const VALUE = `Hello {WORLD} {A} {B} {C}`;
            const WORLD = "World";
            const A = 1;
            const B = 1.0;
            const C = true;
            fn main() { VALUE }
        "#)
    );
}

#[test]
fn test_integer_ops() {
    test_op!(i64 => 1 + 2 = 3);
    test_op!(i64 => 2 - 1 = 1);
    test_op!(i64 => 8 / 2 = 4);
    test_op!(i64 => 8 * 2 = 16);
    test_op!(i64 => 0b1010 << 2 = 0b101000);
    test_op!(i64 => 0b1010 >> 2 = 0b10);
    test_op!(bool => 1 < 2 = true);
    test_op!(bool => 2 < 2 = false);
    test_op!(bool => 1 <= 1 = true);
    test_op!(bool => 2 <= 1 = false);
    test_op!(bool => 3 > 2 = true);
    test_op!(bool => 2 > 2 = false);
    test_op!(bool => 1 >= 1 = true);
    test_op!(bool => 0 >= 2 = false);
}

macro_rules! test_float_op {
    ($ty:ty => $lhs:literal $op:tt $rhs:literal = $result:literal) => {{
        let program = format!(
            r#"const A = {lhs}.0; const B = {rhs}.0; const VALUE = A {op} B; fn main() {{ VALUE }}"#,
            lhs = $lhs, rhs = $rhs, op = stringify!($op),
        );

        assert_eq!(
            $result,
            rune!($ty => &program),
            concat!("expected ", stringify!($result), " out of program `{}`"),
            program
        );
    }}
}

#[test]
fn test_float_ops() {
    test_float_op!(f64 => 1 + 2 = 3f64);
    test_float_op!(f64 => 2 - 1 = 1f64);
    test_float_op!(f64 => 8 / 2 = 4f64);
    test_float_op!(f64 => 8 * 2 = 16f64);
    test_float_op!(bool => 1 < 2 = true);
    test_float_op!(bool => 2 < 2 = false);
    test_float_op!(bool => 1 <= 1 = true);
    test_float_op!(bool => 2 <= 1 = false);
    test_float_op!(bool => 3 > 2 = true);
    test_float_op!(bool => 2 > 2 = false);
    test_float_op!(bool => 1 >= 1 = true);
    test_float_op!(bool => 0 >= 2 = false);
}
