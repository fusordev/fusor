#!/usr/bin/env qjs
const café = 0b1010_0101;
const \u0061 = 1;
const escapedIdentifier = 0o755;
const hexadecimal = 0xFF_FF;
const decimal = 1_000.25e-2;
const bigint = 1_000n;
const string = "\u{1F980}";
const template = `values:${café}:${hexadecimal}`;
const regexp = /[a-z]+/dgimsuy;

void [
    café,
    a,
    escapedIdentifier,
    hexadecimal,
    decimal,
    bigint,
    string,
    template,
    regexp,
];
