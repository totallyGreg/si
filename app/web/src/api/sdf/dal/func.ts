import { ActionKind, ActionPrototypeId } from "@/api/sdf/dal/action";
import { OutputSocketId, SchemaVariantId } from "./schema";
import { ComponentId } from "./component";
import { PropId } from "./prop";

export type FuncArgumentId = string;
export type FuncId = string;
export type AttributePrototypeArgumentId = string;

export enum FuncKind {
  Action = "Action",
  Attribute = "Attribute",
  Authentication = "Authentication",
  CodeGeneration = "CodeGeneration",
  Intrinsic = "Intrinsic",
  Qualification = "Qualification",
  SchemaVariantDefinition = "SchemaVariantDefinition",
  Unknown = "Unknown",
}

export enum CustomizableFuncKind {
  Action = "Action",
  Attribute = "Attribute",
  Authentication = "Authentication",
  CodeGeneration = "CodeGeneration",
  Qualification = "Qualification",
}

// TODO(nick,wendy): this is ugly to use in some places. We probably need to think of a better interface. Blame me, not Wendy.
export function customizableFuncKindToFuncKind(
  customizableFuncKind: CustomizableFuncKind,
): FuncKind {
  switch (customizableFuncKind) {
    case CustomizableFuncKind.Action:
      return FuncKind.Action;
    case CustomizableFuncKind.Attribute:
      return FuncKind.Attribute;
    case CustomizableFuncKind.Authentication:
      return FuncKind.Authentication;
    case CustomizableFuncKind.CodeGeneration:
      return FuncKind.CodeGeneration;
    case CustomizableFuncKind.Qualification:
      return FuncKind.Qualification;
    default:
      throw new Error(
        "this should not be possible since CustomizableFuncKind is a subset of FuncKind",
      );
  }
}

export const CUSTOMIZABLE_FUNC_TYPES = {
  [CustomizableFuncKind.Action]: {
    pluralLabel: "Actions",
    singularLabel: "Action",
  },
  [CustomizableFuncKind.Attribute]: {
    pluralLabel: "Attributes",
    singularLabel: "Attribute",
  },
  [CustomizableFuncKind.Authentication]: {
    pluralLabel: "Authentications",
    singularLabel: "Authentication",
  },
  [CustomizableFuncKind.CodeGeneration]: {
    pluralLabel: "Code Generations",
    singularLabel: "Code Generation",
  },
  [CustomizableFuncKind.Qualification]: {
    pluralLabel: "Qualifications",
    singularLabel: "Qualification",
  },
};

export const isCustomizableFuncKind = (f: FuncKind) =>
  f in CUSTOMIZABLE_FUNC_TYPES;

export enum FuncArgumentKind {
  Array = "Array",
  Boolean = "Boolean",
  Integer = "Integer",
  Json = "Json",
  Object = "Object",
  String = "String",
  Map = "Map",
  Any = "Any",
}

export interface FuncArgument {
  id: string;
  name: string;
  kind: FuncArgumentKind;
  elementKind?: FuncArgumentKind;
}

export interface FuncSummary {
  funcId: FuncId;
  kind: FuncKind;
  name: string;
  display_name: string | null;
  is_locked: boolean;
}

export interface FuncCode {
  funcId: FuncId;
  code: string;
  types: string;
}

export interface AttributeArgumentBinding {
  funcArgumentId: FuncArgumentId | null;
  attributePrototypeArgumentId: AttributePrototypeArgumentId | null;
  propId: PropId | null;
  inputSocketId: OutputSocketId | null;
}

export enum FuncBindingKind {
  Action = "Action",
  Attribute = "Attribute",
  Authentication = "Authentication",
  CodeGeneration = "CodeGeneration",
  Qualification = "Qualification",
}

export interface Action {
  bindingKind: FuncBindingKind.Action;
  // uneditable
  funcId: FuncId | null;
  schemaVariantId: SchemaVariantId | null;
  actionPrototypeId: ActionPrototypeId | null;
  // editable
  kind: ActionKind | null;
}

export interface Attribute {
  bindingKind: FuncBindingKind.Attribute;
  // uneditable
  funcId: FuncId | null;
  actionPrototypeId: ActionPrototypeId | null;
  // needed on create
  schemaVariantId: SchemaVariantId | null;
  componentId: ComponentId | null;
  // editable
  propId: PropId | null;
  outputSocketId: OutputSocketId | null;
  argumentBingindgs: AttributeArgumentBinding[];
}

export interface Authentication {
  bindingKind: FuncBindingKind.Authentication;
  funcId: FuncId;
  schemaVariantId: SchemaVariantId;
}

export interface CodeGeneration {
  bindingKind: FuncBindingKind.CodeGeneration;
  funcId: FuncId | null;
  schemaVariantId: SchemaVariantId | null;
  actionPrototypeId: ActionPrototypeId | null;
  componentId: ComponentId | null;
  // editable
  inputs: LeafInputLocation[];
}

export interface Qualification {
  bindingKind: FuncBindingKind.Qualification;
  funcId: FuncId | null;
  schemaVariantId: SchemaVariantId | null;
  actionPrototypeId: ActionPrototypeId | null;
  componentId: ComponentId | null;
  // editable
  inputs: LeafInputLocation[];
}

export type FuncBinding =
  | Action
  | Attribute
  | Authentication
  | CodeGeneration
  | Qualification;

export type LeafInputLocation =
  | "code"
  | "deletedAt"
  | "domain"
  | "resource"
  | "secrets";
