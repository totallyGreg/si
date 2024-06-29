import { StandardModel } from "@/api/sdf/dal/standard_model";
import { FuncId } from "@/api/sdf/dal/func";
import { Prop } from "@/api/sdf/dal/prop";

export enum SchemaKind {
  Concept = "concept",
  Implementation = "implementation",
  Concrete = "concrete",
}

export interface Schema extends StandardModel {
  name: string;
  kind: SchemaKind;
  ui_menu_name: string;
  ui_menu_category: string;
  ui_hidden: boolean;
}

export type SchemaVariantId = string;
export type SchemaId = string;

export enum ComponentType {
  Component = "component",
  ConfigurationFrameDown = "configurationFrameDown",
  ConfigurationFrameUp = "configurationFrameUp",
  AggregationFrame = "aggregationFrame",
}

export type OutputSocketId = string;

export interface OutputSocket {
  id: OutputSocketId;
  name: string;
  eligible_to_recieve_data: boolean;
}

export type InputSocketId = string;

export interface InputSocket {
  id: InputSocketId;
  name: string;
  eligible_to_send_data: boolean;
}

export interface SchemaVariant {
  schemaVariantId: string;
  schemaName: string;
  displayName: string | null;
  category: string;
  color: string;
  componentType: ComponentType;
  link: string | null;
  description: string | null;

  created_at: IsoDateString;
  updated_at: IsoDateString;

  version: string;
  assetFuncId: FuncId;
  funcIds: FuncId[];
  isLocked: boolean;

  schemaId: SchemaId;

  inputSockets: InputSocket[];
  outputSockets: OutputSocket[];
  props: Prop[];
}
