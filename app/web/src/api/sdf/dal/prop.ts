export type PropId = string;

export enum PropKind {
  Array = "array",
  Boolean = "boolean",
  Integer = "integer",
  Json = "json",
  Object = "object",
  String = "string",
  Map = "map",
}

export interface Prop {
  id: PropId;
  kind: PropKind;
  name: string;
  path: string;
  eligible_to_recieve_data: boolean;
  eligible_to_sennd_data: boolean;
}
