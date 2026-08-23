fn greeting() -> String {
    format!("cx {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", greeting());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_includes_version() {
        assert_eq!(greeting(), format!("cx {}", env!("CARGO_PKG_VERSION")));
    }
}
