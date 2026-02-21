//! Parse an XML document, traverse the DOM, and run XPath queries.
//!
//! Run with: `cargo run --example parse_and_query`

use uppsala::dom::NodeKind;
use uppsala::xpath::XPathValue;
use uppsala::{parse, XPathEvaluator};

fn main() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<library>
  <book isbn="978-0-06-112008-4" category="fiction">
    <title>To Kill a Mockingbird</title>
    <author>Harper Lee</author>
    <year>1960</year>
    <price currency="USD">12.99</price>
  </book>
  <book isbn="978-0-7432-7356-5" category="non-fiction">
    <title>A Brief History of Time</title>
    <author>Stephen Hawking</author>
    <year>1988</year>
    <price currency="USD">15.50</price>
  </book>
  <book isbn="978-0-13-468599-1" category="technical">
    <title>The Rust Programming Language</title>
    <author>Steve Klabnik</author>
    <author>Carol Nichols</author>
    <year>2019</year>
    <price currency="USD">39.99</price>
  </book>
</library>
"#;

    // ── Parse ──
    let mut doc = parse(xml).expect("Failed to parse XML");

    println!("=== DOM Traversal ===\n");

    // Get the document element
    let root = doc.document_element().expect("No document element");
    let root_elem = doc.element(root).unwrap();
    println!("Root element: {}", root_elem.name.local_name);

    // Find all <book> elements
    let books = doc.get_elements_by_tag_name("book");
    println!("Found {} books\n", books.len());

    // Walk each book and print details
    for book_id in &books {
        let book_elem = doc.element(*book_id).unwrap();
        let isbn = book_elem.get_attribute("isbn").unwrap_or("unknown");
        let category = book_elem.get_attribute("category").unwrap_or("unknown");
        println!("Book (isbn={}, category={}):", isbn, category);

        for child_id in doc.children(*book_id) {
            if let Some(NodeKind::Element(child)) = doc.node_kind(child_id) {
                let tag = &child.name.local_name;
                let text = doc.text_content_deep(child_id);
                println!("  {}: {}", tag, text);
            }
        }
        println!();
    }

    // ── XPath Queries ──
    println!("=== XPath Queries ===\n");

    // Prepare the document for XPath (builds virtual attribute nodes)
    doc.prepare_xpath();
    let eval = XPathEvaluator::new();

    let root = doc.root();

    // Query 1: all titles
    println!("All titles (//title):");
    if let Ok(XPathValue::NodeSet(nodes)) = eval.evaluate(&doc, root, "//title") {
        for id in &nodes {
            println!("  - {}", doc.text_content_deep(*id));
        }
    }
    println!();

    // Query 2: fiction books
    println!("Fiction titles (//book[@category='fiction']/title):");
    if let Ok(XPathValue::NodeSet(nodes)) =
        eval.evaluate(&doc, root, "//book[@category='fiction']/title")
    {
        for id in &nodes {
            println!("  - {}", doc.text_content_deep(*id));
        }
    }
    println!();

    // Query 3: count books
    println!("Number of books (count(//book)):");
    if let Ok(XPathValue::Number(n)) = eval.evaluate(&doc, root, "count(//book)") {
        println!("  {}", n);
    }
    println!();

    // Query 4: books published after 1980
    println!("Books published after 1980 (//book[year > 1980]/title):");
    if let Ok(XPathValue::NodeSet(nodes)) = eval.evaluate(&doc, root, "//book[year > 1980]/title") {
        for id in &nodes {
            println!("  - {}", doc.text_content_deep(*id));
        }
    }
    println!();

    // Query 5: string function
    println!("Concatenated author names (//book[1]/author):");
    if let Ok(XPathValue::NodeSet(nodes)) = eval.evaluate(&doc, root, "//author") {
        for id in &nodes {
            let name = doc.text_content_deep(*id);
            println!("  - {} (length: {})", name, name.len());
        }
    }

    // ── Serialization ──
    println!("\n=== Round-trip Serialization ===\n");
    let output = doc.to_xml();
    println!("Compact output length: {} bytes", output.len());

    let pretty = doc.to_xml_with_options(&uppsala::XmlWriteOptions::pretty("  "));
    println!("Pretty output length: {} bytes", pretty.len());
    println!("\nPretty output (first 400 chars):");
    println!("{}", &pretty[..pretty.len().min(400)]);
}
