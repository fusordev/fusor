function defaults(first = 1, second = first) {
    return second;
}

function destructured({ value }, [head], ...rest) {
    return value + head + rest.length;
}

const arrow = (first, ...rest) => first + rest.length;

defaults;
destructured;
arrow;
