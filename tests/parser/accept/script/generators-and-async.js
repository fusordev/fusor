async function asynchronous(value) {
    await value;
}

function* generator(value) {
    yield value;
}

async function* asynchronousGenerator(value) {
    yield await value;
}

asynchronous;
generator;
asynchronousGenerator;
