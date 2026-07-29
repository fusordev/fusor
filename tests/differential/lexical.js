const answer = 0b1010_10 + 0o10 + 0x10 + 1_000;
const greeting = `hello ${"QuickJS"}`;
print(answer);
print(greeting);
print(/q(?:uick)?js/iu.test("QuickJS"));
