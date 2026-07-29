class Counter {
    static #created = 0;
    #value = 0;

    static {
        this.#created += 1;
    }

    get value() {
        return this.#value;
    }

    set value(next) {
        this.#value = next;
    }
}

Counter;
