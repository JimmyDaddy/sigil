import {
  forwardRef,
  useId,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from "react";

interface FieldFrameProps {
  readonly id: string;
  readonly label: string;
  readonly description?: string;
  readonly error?: string;
  readonly children: ReactNode;
}

function FieldFrame({ id, label, description, error, children }: FieldFrameProps) {
  const messageId = `${id}-message`;
  return (
    <div className={`sg-field ${error === undefined ? "" : "sg-field-error"}`}>
      <label className="sg-field-label" htmlFor={id}>{label}</label>
      {children}
      {description !== undefined || error !== undefined ? (
        <small id={messageId}>{error ?? description}</small>
      ) : null}
    </div>
  );
}

interface SharedFieldProps {
  readonly label: string;
  readonly description?: string;
  readonly error?: string;
}

export const TextField = forwardRef<HTMLInputElement, SharedFieldProps & InputHTMLAttributes<HTMLInputElement>>(
  function TextField({ label, description, error, id: suppliedId, className = "", ...props }, ref) {
    const generatedId = useId();
    const id = suppliedId ?? generatedId;
    const describedBy = description !== undefined || error !== undefined ? `${id}-message` : undefined;
    return (
      <FieldFrame id={id} label={label} description={description} error={error}>
        <input
          ref={ref}
          id={id}
          className={`sg-field-control ${className}`.trim()}
          aria-describedby={describedBy}
          aria-invalid={error === undefined ? undefined : true}
          {...props}
        />
      </FieldFrame>
    );
  },
);

export const TextArea = forwardRef<HTMLTextAreaElement, SharedFieldProps & TextareaHTMLAttributes<HTMLTextAreaElement>>(
  function TextArea({ label, description, error, id: suppliedId, className = "", ...props }, ref) {
    const generatedId = useId();
    const id = suppliedId ?? generatedId;
    const describedBy = description !== undefined || error !== undefined ? `${id}-message` : undefined;
    return (
      <FieldFrame id={id} label={label} description={description} error={error}>
        <textarea
          ref={ref}
          id={id}
          className={`sg-field-control ${className}`.trim()}
          aria-describedby={describedBy}
          aria-invalid={error === undefined ? undefined : true}
          {...props}
        />
      </FieldFrame>
    );
  },
);

export const Select = forwardRef<HTMLSelectElement, SharedFieldProps & SelectHTMLAttributes<HTMLSelectElement>>(
  function Select({ label, description, error, id: suppliedId, className = "", children, ...props }, ref) {
    const generatedId = useId();
    const id = suppliedId ?? generatedId;
    const describedBy = description !== undefined || error !== undefined ? `${id}-message` : undefined;
    return (
      <FieldFrame id={id} label={label} description={description} error={error}>
        <select
          ref={ref}
          id={id}
          className={`sg-field-control ${className}`.trim()}
          aria-describedby={describedBy}
          aria-invalid={error === undefined ? undefined : true}
          {...props}
        >
          {children}
        </select>
      </FieldFrame>
    );
  },
);

export function Checkbox({
  label,
  description,
  className = "",
  id: suppliedId,
  "aria-label": ariaLabel,
  ...props
}: { readonly label: string; readonly description?: string } & InputHTMLAttributes<HTMLInputElement>) {
  const generatedId = useId();
  const id = suppliedId ?? generatedId;
  return (
    <label className={`sg-check-control ${className}`.trim()} htmlFor={id}>
      <input id={id} type="checkbox" aria-label={ariaLabel ?? label} {...props} />
      <span><strong>{label}</strong>{description === undefined ? null : <small>{description}</small>}</span>
    </label>
  );
}

export function Radio({
  label,
  description,
  className = "",
  id: suppliedId,
  "aria-label": ariaLabel,
  ...props
}: { readonly label: string; readonly description?: string } & InputHTMLAttributes<HTMLInputElement>) {
  const generatedId = useId();
  const id = suppliedId ?? generatedId;
  return (
    <label className={`sg-check-control ${className}`.trim()} htmlFor={id}>
      <input id={id} type="radio" aria-label={ariaLabel ?? label} {...props} />
      <span><strong>{label}</strong>{description === undefined ? null : <small>{description}</small>}</span>
    </label>
  );
}
