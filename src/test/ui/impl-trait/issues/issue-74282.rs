#![feature(type_alias_impl_trait)]

type Closure = impl Fn() -> u64;
struct Anonymous(Closure);

fn main() {
    let y = || -> Closure { || 3 };
    Anonymous(|| { //~ ERROR mismatched types
        3 //~^ ERROR mismatched types
    })
}
