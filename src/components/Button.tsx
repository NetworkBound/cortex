import { forwardRef, type ButtonHTMLAttributes } from "react";

// Shared button primitive over the existing `.btn-*` design tokens in
// styles/global.css. The base `button` element already carries the neutral
// ("secondary") look — height formula, border, hover/active/disabled states —
// so each variant just layers on the right token class:
//
//   primary   → .btn-primary  (accent fill)
//   secondary → (bare button) the default neutral elevated button
//   danger    → .btn-danger   (danger fill)
//   ghost     → .btn-ghost    (transparent until hover)
//
// Using this instead of hand-writing `<button className="btn-primary">`
// everywhere keeps variants consistent and gives one place to evolve them.

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost";

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  primary: "btn-primary",
  secondary: "",
  danger: "btn-danger",
  ghost: "btn-ghost",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", className, type, children, ...rest },
  ref,
) {
  const classes = [VARIANT_CLASS[variant], className].filter(Boolean).join(" ");
  return (
    <button
      ref={ref}
      // Default to type="button": an unspecified type inside a <form> defaults
      // to "submit" and silently submits/reloads — a classic footgun.
      type={type ?? "button"}
      className={classes || undefined}
      {...rest}
    >
      {children}
    </button>
  );
});
