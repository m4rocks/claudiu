mod common;

use std::path::Path;
use std::time::Instant;

use claudiu_core::SessionId;
use common::*;
use serde_json::json;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const CART: &str = "pub struct Cart {\n    items: Vec<u32>,\n}\n\nimpl Cart {\n    pub fn new() -> Cart {\n        Cart { items: vec![] }\n    }\n\n    pub fn total(&self) -> u32 {\n        self.items.iter().sum()\n    }\n}\n\npub trait Priced {\n    fn price(&self) -> u32;\n}\n";

fn fixture(h: &Harness) -> SessionId {
    let root = h.project();
    write(&root, "src/cart.rs", CART);
    write(&root, "src/main.rs", "mod cart;\nfn main() {\n    let c = cart::Cart::new();\n    println!(\"{}\", c.total());\n}\n");
    write(&root, "src/unrelated.rs", "pub fn lonely() {}\n");
    write(&root, "tests/cart_test.rs", "use shop::Cart;\n#[test]\nfn totals_an_empty_cart() {\n    assert_eq!(Cart::new().total(), 0);\n}\n");
    write(&root, "tests/other_test.rs", "#[test]\nfn nothing_to_do_with_it() {\n    assert!(true);\n}\n");
    write(&root, "web/shop.py", "class Basket:\n    def add(self, item):\n        pass\n\ndef checkout(basket):\n    return basket.add(1)\n");
    write(&root, "web/shop.ts", "export interface Order { id: number }\nexport class Shop {\n  place(o: Order): void {}\n}\nexport function open(): Shop { return new Shop(); }\n");
    write(&root, "web/shop.test.ts", "import { Shop } from './shop';\ntest('places', () => { new Shop().place({ id: 1 }); });\n");
    write(&root, ".gitignore", "target/\nnode_modules/\n");
    write(&root, "target/junk.rs", "pub fn Cart_in_target() {}\n");
    write(&root, "node_modules/dep/index.js", "function Cart() {}\n");
    h.core.start_session(&root).unwrap()
}

fn tool(h: &Harness, id: &SessionId, name: &str, args: serde_json::Value) -> String {
    call_tool(&h.core, id, name, args)
}

#[test]
fn outline_lists_a_files_symbols_with_their_lines() {
    let h = Harness::new(json!({}));
    let id = fixture(&h);

    let out = tool(&h, &id, "outline", json!({"path": "src/cart.rs"}));
    for want in ["struct Cart", "fn new", "fn total", "trait Priced", "fn price"] {
        assert!(out.contains(want), "{want} missing from:\n{out}");
    }
    assert!(out.lines().any(|l| l.contains("fn total") && l.contains("10-12")), "lines of total:\n{out}");
    assert!(out.lines().find(|l| l.contains("fn new")).unwrap().starts_with("    "), "methods nest under their impl:\n{out}");
    assert!(out.len() < 600, "{} bytes", out.len());

    let py = tool(&h, &id, "outline", json!({"path": "web/shop.py"}));
    assert!(py.contains("class Basket") && py.contains("def add") && py.contains("def checkout"), "{py}");
    let ts = tool(&h, &id, "outline", json!({"path": "web/shop.ts"}));
    assert!(ts.contains("interface Order") && ts.contains("class Shop") && ts.contains("function open"), "{ts}");

    assert!(tool(&h, &id, "outline", json!({"path": "nope.rs"})).contains("not in the index"));
    assert!(tool(&h, &id, "outline", json!({"path": "../outside.rs"})).contains("not in the index"));
}

#[test]
fn find_symbol_gives_the_definition_and_where_the_name_is_used() {
    let h = Harness::new(json!({}));
    let id = fixture(&h);

    let out = tool(&h, &id, "find_symbol", json!({"name": "total"}));
    assert!(out.lines().next().unwrap().contains("src/cart.rs:10") && out.contains("fn total"), "{out}");
    assert!(out.contains("src/main.rs:4"), "a use in main:\n{out}");
    assert!(out.contains("tests/cart_test.rs:4"), "and in the test:\n{out}");
    assert!(!out.contains("unrelated"), "{out}");

    let cart = tool(&h, &id, "find_symbol", json!({"name": "Cart"}));
    assert!(!cart.contains("target/") && !cart.contains("node_modules"), "ignored folders stay out:\n{cart}");

    assert!(tool(&h, &id, "find_symbol", json!({"name": "NoSuchThing"})).contains("No symbol"));
}

#[test]
fn repo_map_ranks_by_use_and_stays_inside_its_budget() {
    let h = Harness::new(json!({}));
    let id = fixture(&h);

    let out = tool(&h, &id, "repo_map", json!({}));
    let at = |p: &str| out.find(p).unwrap_or_else(|| panic!("{p} missing:\n{out}"));
    assert!(at("src/cart.rs") < at("src/unrelated.rs"), "what others use comes first:\n{out}");
    assert!(out.contains("Cart") && out.contains("total"), "{out}");
    assert!(!out.contains("target/") && !out.contains("node_modules"), "{out}");

    let sub = tool(&h, &id, "repo_map", json!({"path": "web"}));
    assert!(sub.contains("web/shop.ts") && !sub.contains("src/cart.rs"), "{sub}");

    // A big repository is cut to the budget with a note, never dumped whole.
    let root = h.project();
    for i in 0..400 {
        write(&root, &format!("gen/m{i}.rs"), &format!("pub fn generated_function_{i}() {{}}\npub struct Generated{i};\n"));
    }
    let big = tool(&h, &id, "repo_map", json!({}));
    assert!(big.len() <= 6_500, "{} bytes", big.len());
    assert!(big.lines().last().unwrap().contains("more files"), "{big}");
    assert!(big.contains("src/cart.rs"), "the used files survive the cut");
}

#[test]
fn related_tests_finds_the_tests_that_touch_a_file() {
    let h = Harness::new(json!({}));
    let id = fixture(&h);

    let out = tool(&h, &id, "related_tests", json!({"path": "src/cart.rs"}));
    assert!(out.contains("tests/cart_test.rs"), "{out}");
    assert!(!out.contains("other_test"), "{out}");

    let ts = tool(&h, &id, "related_tests", json!({"path": "web/shop.ts"}));
    assert!(ts.contains("web/shop.test.ts"), "{ts}");

    assert!(tool(&h, &id, "related_tests", json!({"path": "src/unrelated.rs"})).contains("No tests"));
}

#[test]
fn a_change_updates_the_index_without_parsing_everything_again() {
    let h = Harness::new(json!({}));
    let id = fixture(&h);
    tool(&h, &id, "repo_map", json!({}));
    let first = h.core.index_parses(&id);
    assert!(first >= 7, "every source file is parsed once: {first}");

    tool(&h, &id, "repo_map", json!({}));
    assert_eq!(h.core.index_parses(&id), first, "nothing changed, nothing parsed");

    let root = h.project();
    write(&root, "src/unrelated.rs", "pub fn lonely() {}\npub fn brand_new_function() {}\n");
    std::fs::remove_file(root.join("web/shop.py")).unwrap();
    write(&root, "src/added.rs", "pub fn added_later() {}\n");

    assert!(tool(&h, &id, "find_symbol", json!({"name": "brand_new_function"})).contains("src/unrelated.rs:2"));
    assert!(tool(&h, &id, "find_symbol", json!({"name": "added_later"})).contains("src/added.rs:1"));
    assert!(tool(&h, &id, "find_symbol", json!({"name": "checkout"})).contains("No symbol"), "a deleted file leaves the index");
    assert_eq!(h.core.index_parses(&id), first + 2, "only the two changed files were parsed again");
}

#[test]
fn the_tools_need_no_confirmation_and_stay_cheap_to_describe() {
    let h = Harness::new(json!({}));
    let _ = fixture(&h);
    let new = &h.adapter_calls("session/new")[0];
    let allowed = new["_meta"]["claudeCode"]["options"]["allowedTools"].as_array().expect("allowedTools is set").clone();
    assert!(allowed.iter().any(|t| t == "mcp__claudiu"), "the whole claudiu server is pre-allowed: {allowed:?}");
    let id = h.core.start_session(&h.project()).unwrap();
    let listed = mcp(&h.core, &id, "tools/list", json!({}));
    let names: Vec<&str> = listed["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for t in ["repo_map", "outline", "find_symbol", "related_tests", "expand", "request_access"] {
        assert!(names.contains(&t), "{t} is listed: {names:?}");
    }
}

#[test]
fn it_is_fast_and_compact_on_a_real_repository() {
    let h = Harness::new(json!({}));
    // This repository itself: Rust, a few thousand lines, a real .gitignore.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let id = h.core.start_session(&repo).unwrap();

    let started = Instant::now();
    let map = tool(&h, &id, "repo_map", json!({}));
    let cold = started.elapsed();
    assert!(map.len() <= 6_500, "{} bytes of map", map.len());
    assert!(!map.contains("target/"), "build output is not indexed");
    assert!(cold.as_secs() < 20, "first build took {cold:?}");

    let started = Instant::now();
    let found = tool(&h, &id, "find_symbol", json!({"name": "Core"}));
    assert!(started.elapsed().as_millis() < 1500, "a warm query took {:?}", started.elapsed());
    assert!(found.contains("crates/core/src/core.rs"), "{found}");
    assert!(found.len() < 4_000, "{} bytes", found.len());

    let outline = tool(&h, &id, "outline", json!({"path": "crates/core/src/core.rs"}));
    assert!(outline.contains("fn start_session") && outline.len() < 5_000, "{} bytes", outline.len());
}
