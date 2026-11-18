export function add(a: number, b: number): number {
  return a + b;
}

export function unused(x: number): number {
  const doubled = x * 2;
  const shifted = doubled + 1;
  return shifted * shifted;
}

export function isAdult(age: number): boolean {
  return age >= 18;
}
