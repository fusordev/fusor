const key = "computed";
const shorthand = 1;
const [head, ...tail] = [1, 2, 3];
const { nested: renamed = head } = { nested: 4 };
const object = {
    shorthand,
    [key]: 2,
    method() {
        return super.toString;
    },
    get value() {
        return 3;
    },
    set value(next) {
        void next;
    },
    ...{ spread: 4 },
};

void [object, tail, renamed];
