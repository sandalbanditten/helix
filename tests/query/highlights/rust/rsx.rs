fn app() -> Element {
    rsx! {
        div { class: "a {b}", "data-x": 1, onclick: move |_| n += 1,
//      ^ @tag
//            ^ @attribute
//                       ^ @variable
//                            ^ @attribute
//                                         ^ @attribute
//                                                  ^ @keyword.storage.modifier
            MyComponent { id, on_close: move |_| show.set(false) }
//          ^ @constructor
//                        ^ @attribute
//                                                    ^ @function.method
            my_comp {}
//          ^ @constructor
            document::Title { "t" }
//                    ^ @constructor
            my-element {}
//          ^ @tag
//             ^ @tag
            for item in items {
//          ^ @keyword.control.repeat
//                   ^ @keyword.control
                li { "{item}" }
//              ^ @tag
//                     ^ @variable
            }
            if let Some(x) = x() {
//          ^ @keyword.control.conditional
                p { to: Route::Post { id: 1 } }
//                  ^ @attribute
//                             ^ @type
            }
        }
    }
}
