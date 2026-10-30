/** One choice of a single- or multi-select field. */
export type ElicitationOption = Readonly<{ value: string; label: string }>;

/** One renderable field of ACP's restricted elicitation schema. */
export type ElicitationField = Readonly<{
  name: string;
  label: string;
  description?: string;
  required: boolean;
}> & (
  | Readonly<{ kind: "text"; minLength?: number; maxLength?: number; pattern?: RegExp; format?: string; default?: string }>
  | Readonly<{ kind: "select"; options: readonly ElicitationOption[]; default?: string }>
  | Readonly<{ kind: "number"; integer: boolean; minimum?: number; maximum?: number; default?: number }>
  | Readonly<{ kind: "boolean"; default?: boolean }>
  | Readonly<{ kind: "multi"; options: readonly ElicitationOption[]; minItems?: number; maxItems?: number; default?: readonly string[] }>
);

/** Raw form input: numbers stay text until submit so partial input can be typed. */
export type ElicitationValue = string | boolean | readonly string[];

/** A schema the dialog cannot show; the agent is answered with -32602. */
export class UnrenderableSchema extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UnrenderableSchema";
  }
}

type Json = Record<string, unknown>;

const isRecord = (value: unknown): value is Json => typeof value === "object" && value !== null && !Array.isArray(value);
const optionalNumber = (value: unknown): number | undefined => typeof value === "number" && Number.isFinite(value) ? value : undefined;

/** Parsed elicitation form: its fields, their defaults, and submit validation. */
export class ElicitationForm {
  readonly fields: readonly ElicitationField[];

  private constructor(fields: readonly ElicitationField[]) {
    this.fields = fields;
  }

  /** Parses the restricted schema; optional fields it cannot show are left out. */
  static parse(schema: unknown): ElicitationForm {
    if (!isRecord(schema) || (schema.type !== undefined && schema.type !== "object")) {
      throw new UnrenderableSchema("requested schema must be an object schema");
    }
    const properties = isRecord(schema.properties) ? schema.properties : {};
    const required = Array.isArray(schema.required) ? schema.required.filter((name): name is string => typeof name === "string") : [];
    const missing = required.find((name) => !(name in properties));
    if (missing !== undefined) throw new UnrenderableSchema(`required field "${missing}" has no schema`);
    const fields = Object.entries(properties).flatMap(([name, property]): ElicitationField[] => {
      const field = ElicitationForm.field(name, property, required.includes(name));
      if (field === undefined && required.includes(name)) throw new UnrenderableSchema(`required field "${name}" has an unsupported type`);
      return field === undefined ? [] : [field];
    });
    return new ElicitationForm(fields);
  }

  /** Narrows one property schema, or `undefined` for a type the dialog cannot render. */
  private static field(name: string, property: unknown, required: boolean): ElicitationField | undefined {
    if (!isRecord(property)) return undefined;
    const base = {
      name,
      label: typeof property.title === "string" ? property.title : name,
      ...(typeof property.description === "string" ? { description: property.description } : {}),
      required
    };
    if (property.type === "string") {
      if (property.enum !== undefined && property.oneOf !== undefined) throw new UnrenderableSchema(`field "${name}" sets both enum and oneOf`);
      const options = ElicitationForm.options(property.enum, property.oneOf);
      if (options !== undefined) {
        const fallback = typeof property.default === "string" && options.some((option) => option.value === property.default) ? { default: property.default } : {};
        return { ...base, kind: "select", options, ...fallback };
      }
      let pattern: RegExp | undefined;
      try {
        pattern = typeof property.pattern === "string" ? new RegExp(property.pattern, "u") : undefined;
      } catch {
        throw new UnrenderableSchema(`field "${name}" has an invalid pattern`);
      }
      const minLength = optionalNumber(property.minLength);
      const maxLength = optionalNumber(property.maxLength);
      return {
        ...base,
        kind: "text",
        ...(minLength === undefined ? {} : { minLength }),
        ...(maxLength === undefined ? {} : { maxLength }),
        ...(pattern === undefined ? {} : { pattern }),
        ...(typeof property.format === "string" ? { format: property.format } : {}),
        ...(typeof property.default === "string" ? { default: property.default } : {})
      };
    }
    if (property.type === "number" || property.type === "integer") {
      const integer = property.type === "integer";
      const minimum = optionalNumber(property.minimum);
      const maximum = optionalNumber(property.maximum);
      const fallback = optionalNumber(property.default);
      const legal = fallback !== undefined && (!integer || Number.isInteger(fallback)) && (minimum === undefined || fallback >= minimum) && (maximum === undefined || fallback <= maximum);
      return { ...base, kind: "number", integer, ...(minimum === undefined ? {} : { minimum }), ...(maximum === undefined ? {} : { maximum }), ...(legal ? { default: fallback } : {}) };
    }
    if (property.type === "boolean") {
      return { ...base, kind: "boolean", ...(typeof property.default === "boolean" ? { default: property.default } : {}) };
    }
    if (property.type === "array" && isRecord(property.items)) {
      const options = ElicitationForm.options(property.items.enum, property.items.anyOf);
      if (options === undefined) return undefined;
      const minItems = optionalNumber(property.minItems);
      const maxItems = optionalNumber(property.maxItems);
      const fallback = Array.isArray(property.default) ? property.default.filter((value): value is string => options.some((option) => option.value === value)) : undefined;
      return { ...base, kind: "multi", options, ...(minItems === undefined ? {} : { minItems }), ...(maxItems === undefined ? {} : { maxItems }), ...(fallback === undefined ? {} : { default: fallback }) };
    }
    return undefined;
  }

  /** Reads plain `enum` values or titled `{ const, title }` options. */
  private static options(values: unknown, titled: unknown): readonly ElicitationOption[] | undefined {
    if (Array.isArray(values)) return values.filter((value): value is string => typeof value === "string").map((value) => ({ value, label: value }));
    if (Array.isArray(titled)) {
      return titled.flatMap((option) => isRecord(option) && typeof option.const === "string"
        ? [{ value: option.const, label: typeof option.title === "string" ? option.title : option.const }]
        : []);
    }
    return undefined;
  }

  /** Prefills each field with its legal default. */
  initialValues(): Record<string, ElicitationValue> {
    return Object.fromEntries(this.fields.map((field): [string, ElicitationValue] => {
      switch (field.kind) {
        case "boolean": return [field.name, field.default ?? false];
        case "multi": return [field.name, field.default ?? []];
        case "number": return [field.name, field.default === undefined ? "" : String(field.default)];
        default: return [field.name, field.default ?? ""];
      }
    }));
  }

  /** Checks every field and builds the accept content when all pass. */
  validate(values: Readonly<Record<string, ElicitationValue>>): Readonly<{ errors: Readonly<Record<string, string>>; content?: Record<string, unknown> }> {
    const errors: Record<string, string> = {};
    const content: Record<string, unknown> = {};
    for (const field of this.fields) {
      const value = values[field.name];
      const empty = value === undefined || value === "" || (Array.isArray(value) && value.length === 0);
      if (field.kind !== "boolean" && empty) {
        if (field.required) errors[field.name] = "必填";
        continue;
      }
      const error = ElicitationForm.check(field, value);
      if (error !== undefined) {
        errors[field.name] = error;
        continue;
      }
      content[field.name] = field.kind === "number" ? Number(value) : value;
    }
    return Object.keys(errors).length === 0 ? { errors, content } : { errors };
  }

  /** Returns why one non-empty value breaks its field's constraints. */
  private static check(field: ElicitationField, value: ElicitationValue | undefined): string | undefined {
    switch (field.kind) {
      case "text": {
        const text = String(value);
        // JSON Schema measures Unicode code points, not UTF-16 code units.
        const length = [...text].length;
        if (field.minLength !== undefined && length < field.minLength) return `至少 ${field.minLength} 个字符`;
        if (field.maxLength !== undefined && length > field.maxLength) return `最多 ${field.maxLength} 个字符`;
        if (field.pattern !== undefined && !field.pattern.test(text)) return "格式不符合要求";
        if (field.format === "email" && !/^[^\s@]+@[^\s@]+$/u.test(text)) return "需要邮箱地址";
        if (field.format === "uri" && !URL.canParse(text)) return "需要完整 URL";
        if (field.format === "date" && !/^\d{4}-\d{2}-\d{2}$/u.test(text)) return "需要日期 YYYY-MM-DD";
        if (field.format === "date-time" && (!text.includes("T") || Number.isNaN(Date.parse(text)))) return "需要 ISO 8601 日期时间";
        return undefined;
      }
      case "number": {
        const number = Number(value);
        if (!Number.isFinite(number)) return "需要数字";
        if (field.integer && !Number.isInteger(number)) return "需要整数";
        if (field.minimum !== undefined && number < field.minimum) return `不小于 ${field.minimum}`;
        if (field.maximum !== undefined && number > field.maximum) return `不大于 ${field.maximum}`;
        return undefined;
      }
      case "multi": {
        const count = Array.isArray(value) ? value.length : 0;
        if (field.minItems !== undefined && count < field.minItems) return `至少选择 ${field.minItems} 项`;
        if (field.maxItems !== undefined && count > field.maxItems) return `最多选择 ${field.maxItems} 项`;
        return undefined;
      }
      default: return undefined;
    }
  }
}
