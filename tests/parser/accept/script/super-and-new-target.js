class Base {
    constructor(value) {
        this.value = value;
    }

    method() {
        return this.value;
    }
}

class Derived extends Base {
    constructor(value) {
        super(value);
    }

    method() {
        return super.method();
    }
}

function constructorContext() {
    return new.target;
}

void [Derived, constructorContext];
