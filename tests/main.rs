//! The HTTP-level tests, built as one test program (`cargo test --test integration`): each file
//! of `tests/` is a module of it, so the service and its dependencies are linked once, not once
//! per file. Run one file's tests with its module name as the filter, such as
//! `cargo test --test integration graph_testset::`.

mod common;

mod acquisition;
mod acquisition_live;
mod bench;
mod convert;
mod crawl_live;
mod deletion;
mod dev;
mod documents;
mod graph;
mod graph_draft;
mod graph_switch;
mod graph_testset;
mod isolation;
mod jwt;
mod librarian;
mod librarian_live;
mod lint;
mod lint_eval;
mod mcp;
mod meaning;
mod meaning_testset;
mod personal;
mod search;
mod search_testset;
mod testset;
mod traces;
