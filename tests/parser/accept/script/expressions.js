const base = {
    nested: { value: 1 },
    method(value) {
        return value;
    },
};
let assigned = 0;
assigned ||= 1;
assigned &&= 2;
assigned ??= 3;

const optional = base?.nested?.["value"] ?? 0;
const called = base.method?.(optional);
const array = [0, ...[1, 2]];
const object = { ...base, called };
const constructed = new class {};
const arithmetic = ((2 ** 3) * 4 + 1, true ? 33 : 0);
const tagged = ((parts, value) => [parts, value])`value:${arithmetic}`;

delete (null)?.value;
void [assigned, array, object, constructed, tagged];
