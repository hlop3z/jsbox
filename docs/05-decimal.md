# 5. `$` — Exact Decimal Math 💵

[← Back to the guide](README.md)

Remember the decimal problem (`0.1 + 0.2 = 0.30000000000000004`)? Here's the fix.

jsbox gives you a built-in helper called **`$`** (you can also write `Decimal`) that does
**exact** decimal math — no tiny rounding mistakes. It's **always on**, so you don't need
any config. Perfect for money. 🎉

> Under the hood it uses the **same exact-decimal engine** that reads `NUMERIC` columns
> from the database, so the numbers match perfectly.

## Make a decimal

Wrap a value with `$(...)`. Use a **string** for perfect accuracy:

```js
var price = $("19.99"); // ✅ exact
var qty = $(3); // numbers work too
```

> 💡 Tip: `$("0.1")` is exact. `$(0.1)` is _usually_ fine, but `$(0.1 + 0.2)` is already
> broken **before** `$` sees it — so prefer strings when you can.

## Do the math (with methods, not `+`)

⚠️ **Important:** you can't use `+ - * /` symbols on a `$` decimal — JavaScript won't let
us make those exact. Use **methods** instead:

```js
var total = $("19.99").mul(3).add("0.01"); // 19.99 × 3 + 0.01
total.toString(); // "59.98"  ✅ exact!
```

The methods **chain** — each one returns a new decimal you can keep working with.

| Method           | Means         | Example                          |
| ---------------- | ------------- | -------------------------------- |
| `.add(x)`        | plus          | `$("1.10").add("0.20")` → `1.30` |
| `.sub(x)`        | minus         | `$("5").sub("1.50")` → `3.50`    |
| `.mul(x)`        | times         | `$("19.99").mul(3)` → `59.97`    |
| `.div(x)`        | divided by    | `$("10").div(4)` → `2.5`         |
| `.neg()`         | flip the sign | `$("5").neg()` → `-5`            |
| `.abs()`         | make positive | `$("-5").abs()` → `5`            |
| `.round(places)` | round it      | `$("19.985").round(2)` → `19.99` |

`.round` rounds **half-up** (the normal way you learned in school: `19.985` → `19.99`).

## Compare two decimals

```js
$("19.99").gt("9.99"); // true   (greater than)
$("5.00").eq("5"); // true   (equal)
$("1.50").lt("2"); // true   (less than)
```

| Method      | Asks…                   |
| ----------- | ----------------------- |
| `.eq(x)`    | equal?                  |
| `.lt(x)`    | less than?              |
| `.lte(x)`   | less than or equal?     |
| `.gt(x)`    | greater than?           |
| `.gte(x)`   | greater than or equal?  |
| `.isZero()` | is it zero?             |
| `.cmp(x)`   | gives `-1`, `0`, or `1` |

## Getting your answer out

- **`.toString()`** → the exact text, like `"59.98"`. Use this to show it or save it.
- **`.toNumber()`** → a normal JS number (⚠️ can round — only for display/quick stuff).
- In `json(...)`, a `$` decimal turns into its exact string **automatically**:

```js
function handler(ctx) {
  var total = $("19.99").mul(ctx.qty);
  return json({ total: total }, null); // -> { "total": "39.98" }  (already a string!)
}
```

## A full money example 🛒

```js
function handler(ctx) {
  // ctx = { items: [ { price: "19.99", qty: 2 }, { price: "4.50", qty: 3 } ] }
  var total = $("0");
  for (var i = 0; i < ctx.items.length; i++) {
    var item = ctx.items[i];
    total = total.add($(item.price).mul(item.qty));
  }
  var withTax = total.mul("1.08").round(2); // add 8% tax, round to cents

  return json(
    {
      subtotal: total.toString(), // "53.48"
      total_with_tax: withTax.toString(), // "57.76"
    },
    null,
  );
}
```

## Works great with the database 🗄️

Decimals from the database arrive as strings (see
[Talk to a Database](03-database.md)). Wrap them in `$` to do exact math,
then send the result back as a string:

```js
function handler(ctx) {
  var row = db.query("SELECT price FROM products WHERE id = $1", [ctx.id])
    .rows[0];
  var newPrice = $(row.price).mul("1.10").round(2); // +10%, rounded
  db.execute("UPDATE products SET price = $1 WHERE id = $2", [
    newPrice.toString(),
    ctx.id,
  ]);
  return json({ price: newPrice }, null);
}
```

## Good to know

- **Always on** — no `config` needed. `$` and `Decimal` are the same thing.
- Holds about **28–29 digits** — plenty for money and counting. (Not for giant science numbers.)
- Dividing by zero, or a number too big to hold, **throws an error** (catch it with `try/catch`).

## Cheat sheet 📝

- `$("19.99")` makes an exact decimal.
- Use **methods** (`.add .sub .mul .div`), **not** `+ - * /`.
- `.toString()` to show/save, `.round(2)` for cents.
- In `json(...)`, decimals become exact strings for free.

**Next:** [Signed Upload & Download Links →](06-s3.md)

[← Back to the guide](README.md)
