let first = 0;
let second = 0;
const target = { property: 0 };

first++;
++first;
second--;
--second;

[first, second] = [second, first];
({ property: target.property } = { property: first });
[target.property, ...target.rest] = [second, first];

const logical = (first && second) || (first ?? second);
void [logical, target];
