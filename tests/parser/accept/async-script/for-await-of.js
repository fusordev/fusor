async function* values() {
    yield 1;
}

for await (const value of values()) {
    void value;
}
