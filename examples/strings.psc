// CAIE string positions are one-based.
// Run: cargo run -- run examples/strings.psc
DECLARE text : STRING
text <- "Cambridge"
OUTPUT MID(text, 1, 3), MID(text, 4, 3), RIGHT(text, 3), LENGTH(text)
