function handler(ctx) {
  // $sys.env holds plain, returnable config values.
  // $sys.secrets holds credentials as OPAQUE HANDLES -- usable in your logic, but the
  // plaintext never enters JS. Every coercion yields "[secret:NAME]", never the value.
  var region = $sys.env.REGION;
  var tier = $sys.env.TIER;

  // Use a secret to do real work (here: sign something with it). The handle is
  // accepted as an HMAC key; Rust resolves the plaintext to sign, one-way.
  var token = $sys.secrets.API_TOKEN;
  var signed = $sys.crypto.hmac("sha256", token, "payload-to-sign");

  // Trying to ENCODE a secret (a reversible escape hatch) throws -- proving the
  // handle can't be turned back into plaintext through the crypto helpers.
  var encodeBlocked = false;
  try {
    $sys.crypto.base64.encode(token);
  } catch (e) {
    encodeBlocked = true;
  }

  return json(
    {
      region: region,
      tier: tier,
      signed: signed,
      encodeBlocked: encodeBlocked, // -> true
      missingEnv: $sys.env.NOPE === undefined, // unset keys are just undefined
      // Deliberately try to leak a secret -- coercion yields the placeholder, not the value:
      leakedRaw: token, // -> "[secret:API_TOKEN]"
      leakedTemplate: "" + token, // -> "[secret:API_TOKEN]"
      leakedNested: { deep: [token] }, // -> { deep: ["[secret:API_TOKEN]"] }
    },
    null
  );
}
