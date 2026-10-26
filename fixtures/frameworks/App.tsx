import { useState } from "react";

type GreetingProps = {
  name: string;
};

function Greeting({ name }: GreetingProps) {
  return <h1 className="title">Hello, {name}!</h1>;
}

export function App() {
  const [count, setCount] = useState(0);

  return (
    <main>
      <Greeting name="PHP" />
      <button onClick={() => setCount((value) => value + 1)}>
        clicks: {count}
      </button>
    </main>
  );
}
