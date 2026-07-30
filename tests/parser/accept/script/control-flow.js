let result = 0;

outer: for (const value of [0, 1, 2]) {
    if (value === 0) {
        continue;
    }
    while (true) {
        result += value;
        break outer;
    }
}

for (const key in { one: 1 }) {
    result += key.length;
}

do {
    result += 1;
} while (false);

switch (result) {
    case 3:
        result += 1;
        break;
    default:
        result = 0;
}

try {
    throw result;
} catch {
    result += 1;
} finally {
    result += 1;
}

debugger;
void result;
