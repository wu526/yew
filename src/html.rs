//! The main module which contents aliases to necessary items
//! to create a template and implement `update` and `view` functions.

use std::rc::Rc;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::mpsc::{Sender, Receiver, channel};
use stdweb::Value;
use stdweb::web::{Element, INode, EventListenerHandle, document};
use stdweb::web::event::{IMouseEvent, IKeyboardEvent};
use virtual_dom::{VNode, Listener};
use component::Component;

/// Removes anything from the given element.
fn clear_element(element: &Element) {
    while let Some(child) = element.last_child() {
        element.remove_child(&child).expect("can't remove a child");
    }
}

/// Shared reference to a context.
pub type SharedContext<CTX> = Rc<RefCell<CTX>>;

/// Local reference to application internals: messages sender and context.
// TODO Rename to Context
pub struct LocalSender<'a, CTX: 'a, MSG: 'a> {
    context: &'a mut CTX,
    tx: &'a mut Sender<MSG>,
    bind: &'a Value,
}

impl<'a, CTX: 'a, MSG: 'a> Deref for LocalSender<'a, CTX, MSG> {
    type Target = CTX;

    fn deref(&self) -> &CTX {
        &self.context
    }
}

impl<'a, CTX: 'a, MSG: 'a> DerefMut for LocalSender<'a, CTX, MSG> {
    fn deref_mut(&mut self) -> &mut CTX {
        &mut self.context
    }
}

/// A universal callback prototype.
pub type Callback<IN> = Box<Fn(IN)>;

impl<'a, CTX: 'a, MSG: 'static> LocalSender<'a, CTX, MSG> {
    /// This method sends messages back to the component's loop.
    pub fn send_back<F, IN>(&mut self, function: F) -> Callback<IN>
    where
        F: Fn(IN) -> MSG + 'static,
    {
        let sender = self.tx.clone();
        let bind = self.bind.clone();
        let closure = move |input| {
            let output = function(input);
            sender.send(output).expect("App lost the receiver!");
            let bind = bind.clone();
            js! {
                // Schedule to call the loop handler
                // IMPORTANT! If call loop function immediately
                // it stops handling other messages and the first
                // one will be fired.
                var bind = @{bind};
                setTimeout(bind.loop);
            }
        };
        Box::new(closure)
    }
}

/// This class keeps a sender to a context to send a messages to a loop
/// and to schedule the next update call.
pub struct AppSender<CTX, MSG> {
    context: SharedContext<CTX>,
    tx: Sender<MSG>,
    bind: Value,
}

impl<CTX, MSG> Clone for AppSender<CTX, MSG> {
    fn clone(&self) -> Self {
        AppSender {
            tx: self.tx.clone(),
            context: self.context.clone(),
            bind: self.bind.clone(),
        }
    }
}

impl<CTX, MSG> AppSender<CTX, MSG> {
    /// Send the message and schedule an update.
    // TODO Consider to remove this method
    pub fn send(&mut self, msg: MSG) {
        self.tx.send(msg).expect("App lost the receiver!");
        let bind = &self.bind;
        js! {
            // Schedule to call the loop handler
            // IMPORTANT! If call loop function immediately
            // it stops handling other messages and the first
            // one will be fired.
            var bind = @{bind};
            setTimeout(bind.loop);
        }
    }

    /// Clones shared context.
    pub fn context(&self) -> SharedContext<CTX> {
        self.context.clone()
    }
}

/// A context which contains a bridge to send a messages to a loop.
/// Mostly services uses it.
pub struct App<CTX, MSG> {
    context: SharedContext<CTX>,
    tx: Sender<MSG>,
    bind: Value,
    rx: Option<Receiver<MSG>>,
}

impl<CTX: 'static, MSG: 'static> App<CTX, MSG> {
    /// Creates a context with connected sender and receiver.
    pub fn new(context: SharedContext<CTX>) -> Self {
        let bind = js! {
            return { "loop": function() { } };
        };
        let (tx, rx) = channel();
        App {
            tx,
            rx: Some(rx),
            context,
            bind,
        }
    }

    /// Returns a cloned sender.
    pub fn sender(&mut self) -> AppSender<CTX, MSG> {
        AppSender {
            tx: self.tx.clone(),
            context: self.context.clone(),
            bind: self.bind.clone(),
        }
    }

    /// Alias to `mount_to("body", ...)`.
    pub fn mount<COMP>(&mut self, component: COMP)
    where
        COMP: Component<CTX, Msg=MSG> + 'static,
    {
        let element = document().query_selector("body")
            .expect("can't get body node for rendering");
        self.mount_to(element, component)
    }

    /// The main entrypoint of a yew program. It works similar as `program`
    /// function in Elm. You should provide an initial model, `update` function
    /// which will update the state of the model and a `view` function which
    /// will render the model to a virtual DOM tree.
    pub fn mount_to<COMP>(&mut self, element: Element, mut component: COMP)
    where
        COMP: Component<CTX, Msg=MSG> + 'static,
    {
        clear_element(&element);
        //
        let mut sender = self.sender();
        {
            // TODO DRY
            let tx = &mut sender.tx;
            let bind = &sender.bind;
            let mut context = sender.context.borrow_mut();
            let sender = LocalSender {
                tx, bind,
                context: &mut *context,
            };
            component.initialize(sender);
        }
        // No messages at start
        let mut messages = Vec::new();
        let mut last_frame = VNode::from(component.view());
        last_frame.apply(&element, None, self.sender());
        let mut last_frame = Some(last_frame);
        let rx = self.rx.take().expect("application runned without a receiver");
        let bind = self.bind.clone();
        let mut callback = move || {
            messages.extend(rx.try_iter());
            for msg in messages.drain(..) {
                // TODO DRY
                let tx = &mut sender.tx;
                let bind = &sender.bind;
                let mut context = sender.context.borrow_mut();
                let sender = LocalSender {
                    tx, bind,
                    context: &mut *context,
                };
                component.update(msg, sender);
            }
            let mut next_frame = VNode::from(component.view());
            next_frame.apply(&element, last_frame.take(), sender.clone());
            last_frame = Some(next_frame);
        };
        // Initial call for first rendering
        callback();
        js! {
            var bind = @{bind};
            var callback = @{callback};
            bind.loop = callback;
        }
        // TODO `Drop` should drop the callback
    }
}

/// A type which expected as a result of `view` function implementation.
pub type Html<CTX, MSG> = VNode<CTX, MSG>;

macro_rules! impl_action {
    ($($action:ident($event:ident : $type:ident) -> $ret:ty => $convert:expr)*) => {$(
        /// An abstract implementation of a listener.
        pub mod $action {
            use stdweb::web::{IEventTarget, Element};
            use stdweb::web::event::{IEvent, $type};
            use super::*;

            /// A wrapper for a callback.
            /// Listener extracted from here when attached.
            pub struct Wrapper<F>(Option<F>);

            /// And event type which keeps the returned type.
            pub type Event = $ret;

            impl<F, MSG> From<F> for Wrapper<F>
            where
                MSG: 'static,
                F: Fn($ret) -> MSG + 'static,
            {
                fn from(handler: F) -> Self {
                    Wrapper(Some(handler))
                }
            }

            impl<T, CTX: 'static, MSG> Listener<CTX, MSG> for Wrapper<T>
            where
                MSG: 'static,
                T: Fn($ret) -> MSG + 'static,
            {
                fn kind(&self) -> &'static str {
                    stringify!($action)
                }

                fn attach(&mut self, element: &Element, mut sender: AppSender<CTX, MSG>)
                    -> EventListenerHandle {
                    let handler = self.0.take().expect("tried to attach listener twice");
                    let this = element.clone();
                    let listener = move |event: $type| {
                        debug!("Event handler: {}", stringify!($type));
                        event.stop_propagation();
                        let handy_event: $ret = $convert(&this, event);
                        let msg = handler(handy_event);
                        sender.send(msg);
                    };
                    element.add_event_listener(listener)
                }
            }
        }
    )*};
}

// Inspired by: http://package.elm-lang.org/packages/elm-lang/html/2.0.0/Html-Events
impl_action! {
    onclick(event: ClickEvent) -> MouseData => |_, event| { MouseData::from(event) }
    ondoubleclick(event: DoubleClickEvent) -> MouseData => |_, event| { MouseData::from(event) }
    onkeypress(event: KeypressEvent) -> KeyData => |_, event| { KeyData::from(event) }
    /* TODO Add PR to https://github.com/koute/stdweb
    onmousedown(event: MouseDownEvent) -> () => |_, _| { () }
    onmouseup(event: MouseUpEvent) -> () => |_, _| { () }
    onmouseenter(event: MouseEnterEvent) -> () => |_, _| { () }
    onmouseleave(event: MouseLeaveEvent) -> () => |_, _| { () }
    onmouseover(event: MouseOverEvent) -> () => |_, _| { () }
    onmouseout(event: MouseOutEvent) -> () => |_, _| { () }
    */
    oninput(event: InputEvent) -> InputData => |this: &Element, _| {
        use stdweb::web::html_element::InputElement;
        use stdweb::unstable::TryInto;
        let input: InputElement = this.clone().try_into().expect("only an InputElement can have an oninput event listener");
        let value = input.value().into_string().unwrap_or_else(|| "".into());
        InputData { value }
    }
}

/// A type representing data from `onclick` and `ondoubleclick` event.
#[derive(Debug)]
pub struct MouseData {
    /// The screenX is a read-only property of the
    /// [MouseEvent](https://developer.mozilla.org/en-US/docs/Web/API/MouseEvent/screenX)
    /// property which provides the horizontal coordinate (offset)
    /// of the mouse pointer in global (screen) coordinates.
    pub screen_x: f64,
    /// The screenY is a read-only property of the
    /// [MouseEvent](https://developer.mozilla.org/en-US/docs/Web/API/MouseEvent/screenY)
    /// property which provides the vertical coordinate (offset)
    /// of the mouse pointer in global (screen) coordinates.
    pub screen_y: f64,
    /// The clientX is a read-only property of the
    /// [MouseEvent](https://developer.mozilla.org/en-US/docs/Web/API/MouseEvent/clientX)
    /// interface which provides the horizontal coordinate within
    /// the application's client area at which the event occurred
    pub client_x: f64,
    /// The clientY is a read-only property of the
    /// [MouseEvent](https://developer.mozilla.org/en-US/docs/Web/API/MouseEvent/clientX)
    /// interface which provides the vertical coordinate within
    /// the application's client area at which the event occurred
    pub client_y: f64,
}

impl<T: IMouseEvent> From<T> for MouseData {
    fn from(event: T) -> Self {
        MouseData {
            screen_x: event.screen_x(),
            screen_y: event.screen_y(),
            client_x: event.client_x(),
            client_y: event.client_y(),
        }
    }
}

/// A type representing data from `oninput` event.
#[derive(Debug)]
pub struct InputData {
    /// Inserted characters. Contains value from
    /// [InputEvent](https://developer.mozilla.org/en-US/docs/Web/API/InputEvent/data).
    pub value: String,
}

/// A type representing data from `onkeypress` event.
#[derive(Debug)]
pub struct KeyData {
    /// Value of a pressed key. Contains key name from
    /// [KeyboardEvent](https://developer.mozilla.org/en-US/docs/Web/API/KeyboardEvent/key).
    pub key: String,
}

impl<T: IKeyboardEvent> From<T> for KeyData {
    fn from(event: T) -> Self {
        KeyData { key: event.key() }
    }
}

/// A bridging type for checking `href` attribute value.
#[derive(Debug)]
pub struct Href {
    link: String,
}

impl From<String> for Href {
    fn from(link: String) -> Self {
        Href { link }
    }
}

impl<'a> From<&'a str> for Href {
    fn from(link: &'a str) -> Self {
        Href { link: link.to_owned() }
    }
}

impl ToString for Href {
    fn to_string(&self) -> String {
        self.link.to_owned()
    }
}

