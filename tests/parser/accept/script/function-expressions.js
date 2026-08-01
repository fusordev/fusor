const anonymous = function (value) {
    return value;
};
const named = function inner(value) {
    return value === 0 ? 0 : inner(value - 1);
};

void [anonymous, named, (function () {})];
