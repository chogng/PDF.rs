document(version: "1.7") {
  object(1) = catalog(pages: ref(2));
  object(2) = pages(kids: [ref(3)], count: 1);
  object(3) = page(
    media_box: [0, 0, 200, 200],
    resources: {},
    contents: ref(4)
  );
  stream(4) { "q\nQ\n" }
  xref(kind: table);
}

# The M0 generator currently owns this fixed template directly. This source file
# records the intended semantic input until the general DSL parser is implemented.
