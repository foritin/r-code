// draft-07 子集校验器：只支持 codex app-server 生成 schema 实际用到的
// 关键字（type/required/properties/items/enum/const/additionalProperties/
// oneOf/minimum/$ref 内联后不应再出现）。零依赖，供 fixture 检查与后续
// harness 断言复用；不支持的关键字一律报错而不是静默放行。

const TYPE_CHECKS = {
  object: (v) => v !== null && typeof v === "object" && !Array.isArray(v),
  array: Array.isArray,
  string: (v) => typeof v === "string",
  number: (v) => typeof v === "number" && Number.isFinite(v),
  integer: (v) => typeof v === "number" && Number.isInteger(v),
  boolean: (v) => typeof v === "boolean",
  null: (v) => v === null,
};

export function validateAgainstSchema(instance, schema, path = "$", errors = []) {
  if (schema === true) {
    return errors;
  }
  if (schema === false) {
    errors.push(`${path}: schema forbids any value`);
    return errors;
  }
  if (typeof schema !== "object" || schema === null) {
    errors.push(`${path}: invalid schema node`);
    return errors;
  }
  if (Object.prototype.hasOwnProperty.call(schema, "$ref")) {
    errors.push(`${path}: unresolved $ref ${schema.$ref} (fixture schemas must be inlined)`);
    return errors;
  }

  if (schema.type !== undefined) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    const matched = types.some((t) => TYPE_CHECKS[t]?.(instance) ?? false);
    if (!matched) {
      errors.push(`${path}: expected type ${JSON.stringify(schema.type)}, got ${describe(instance)}`);
      return errors;
    }
  }

  if (schema.const !== undefined && instance !== schema.const) {
    errors.push(`${path}: expected const ${JSON.stringify(schema.const)}, got ${JSON.stringify(instance)}`);
  }
  if (schema.enum !== undefined && !schema.enum.some((candidate) => candidate === instance)) {
    errors.push(`${path}: value ${JSON.stringify(instance)} not in enum ${JSON.stringify(schema.enum)}`);
  }
  if (schema.minimum !== undefined && typeof instance === "number" && instance < schema.minimum) {
    errors.push(`${path}: ${instance} < minimum ${schema.minimum}`);
  }

  if (Array.isArray(instance) && schema.items !== undefined) {
    instance.forEach((item, index) => {
      validateAgainstSchema(item, schema.items, `${path}[${index}]`, errors);
    });
  }

  if (TYPE_CHECKS.object(instance)) {
    for (const key of schema.required ?? []) {
      if (!Object.prototype.hasOwnProperty.call(instance, key)) {
        errors.push(`${path}: missing required property "${key}"`);
      }
    }
    const properties = schema.properties ?? {};
    for (const [key, value] of Object.entries(instance)) {
      const propertySchema = properties[key];
      if (propertySchema !== undefined) {
        validateAgainstSchema(value, propertySchema, `${path}.${key}`, errors);
      } else if (schema.additionalProperties === false) {
        errors.push(`${path}: unexpected property "${key}"`);
      } else if (schema.additionalProperties && typeof schema.additionalProperties === "object") {
        validateAgainstSchema(value, schema.additionalProperties, `${path}.${key}`, errors);
      }
    }
  }

  if (schema.oneOf !== undefined) {
    const branches = schema.oneOf.map((branch) => {
      const branchErrors = [];
      validateAgainstSchema(instance, branch, path, branchErrors);
      return branchErrors;
    });
    const valid = branches.filter((branchErrors) => branchErrors.length === 0);
    if (valid.length !== 1) {
      errors.push(`${path}: matched ${valid.length} oneOf branches (expected exactly 1)`);
    }
  }

  return errors;
}

function describe(instance) {
  if (instance === null) {
    return "null";
  }
  if (Array.isArray(instance)) {
    return "array";
  }
  return typeof instance;
}
