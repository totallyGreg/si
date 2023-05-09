async function deleteResource(component: Input): Promise<Output> {
  const resource = component.properties.resource?.payload;

  if (!resource.InstanceId)
    return {
      status: "error",
      payload: resource,
      message: "No EC2 instance id found",
    };

  // Now, delete the Ec2 Instance.
  const child = await siExec.waitUntilEnd("aws", [
    "ec2",
    "terminate-instances",
    "--region",
    component.properties.domain.region,
    "--instance-ids",
    resource.InstanceId,
  ]);

  if (child.exitCode !== 0) {
    console.error(child.stderr);
    return {
      status: "error",
      payload: resource,
      message: `Unable to delete Ec2 Instance, AWS CLI 2 exited with non zero code: ${child.exitCode}`,
    };
  }

  return { payload: null, status: "ok" };
}
