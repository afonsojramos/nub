enum Color { Red, Green }
const greet = (name: string): string => `HELLO_TS:${name}:${Color.Green}`;
console.log(greet("nub"));
