function* generator() {
    function nested() {}
    yield 1
    const arrow = value => value + 1
    yield arrow(1);
}

async function asynchronous() {
    function nested() {}
    await 1
    const arrow = async value => value + 1
    await arrow(1);
}

const empty = () => {}
(() => {});
const object = value => ({ value });

void [generator, asynchronous, empty, object];
