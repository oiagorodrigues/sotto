import { Icon } from "../../components/Icon";

interface Props {
  steps: readonly string[];
  current: number;
}

export function Stepper({ steps, current }: Props) {
  return (
    <nav aria-label="Setup steps">
      <ol className="steps">
        {steps.map((label, i) => {
          const state = i < current ? "done" : i === current ? "cur" : "next";
          const word = state === "done" ? "Done" : state === "cur" ? "In progress" : "Next";
          return (
            <li key={label} className={state} aria-current={state === "cur" ? "step" : undefined}>
              <span className="n">
                {state === "done" ? <Icon name="check" size={14} /> : i + 1}
              </span>
              <span className="tx">
                {label}
                <small>{word}</small>
              </span>
              <span className="sr">{`${label}, ${word}`}</span>
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
