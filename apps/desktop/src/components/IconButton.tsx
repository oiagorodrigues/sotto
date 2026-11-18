import type { ButtonHTMLAttributes } from "react";
import { Icon, type IconName } from "./Icon";
import { Tooltip } from "./Tooltip";

interface Props extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> {
  icon: IconName;
  label: string;
  shortcut?: string;
}

export function IconButton({ icon, label, shortcut, ...rest }: Props) {
  return (
    <Tooltip label={label} shortcut={shortcut}>
      <button type="button" className="icon-btn" aria-label={label} {...rest}>
        <Icon name={icon} />
      </button>
    </Tooltip>
  );
}
