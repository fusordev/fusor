let value = 0;
outer: inner: while (value < 1) {
  value++;
  continue outer;
}
