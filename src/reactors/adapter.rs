
use std::fmt;
use std;
use std::rc::Rc;
use std::collections::HashMap;
use std::io::{self, Result, Error, ErrorKind};
use std::time::Duration;
use mio::{Evented, Token, Ready, PollOpt};
use mio::timer::{Timer, Timeout};
use network::endpoint::{self, EndpointRegistrar, SocketId, EndpointId, EndpointTmpl, EndpointSpec};
use network::transport::{self, Pipe, Acceptor, Transport, Destination};
use network::message::Message;
use network::tcp::pipe;
use network::tcp::acceptor;
use reactors::dispatcher::{self, Scheduled, Scheduler, Schedulable};
use reactors::bus::EventLoopBus;
use reactors::api::{Signal, Task};
use reactors::sequence::Sequence;
use reactors::event_loop::EventLoop;

pub trait Network {
    fn connect(&mut self, sid: SocketId, tmpl: &EndpointTmpl) -> Result<EndpointId>;
    fn reconnect(&mut self, sid: SocketId, eid: EndpointId, tmpl: &EndpointTmpl) -> Result<()>;
    fn bind(&mut self, sid: SocketId, tmpl: &EndpointTmpl) -> Result<EndpointId>;
    fn rebind(&mut self, sid: SocketId, eid: EndpointId, tmpl: &EndpointTmpl) -> Result<()>;
    fn open(&mut self, eid: EndpointId, remote: bool);
    fn close(&mut self, eid: EndpointId, remote: bool);
    fn send(&mut self, eid: EndpointId, msg: Rc<Message>);
    fn recv(&mut self, eid: EndpointId);
}

pub trait Registrar {
    fn register(&mut self,
                io: &Evented,
                tok: Token,
                interest: Ready,
                opt: PollOpt)
                -> io::Result<()>;
    fn reregister(&mut self,
                  io: &Evented,
                  tok: Token,
                  interest: Ready,
                  opt: PollOpt)
                  -> io::Result<()>;
    fn deregister(&mut self, io: &Evented) -> io::Result<()>;
}

pub struct SocketEventLoopContext<'a> {
    socket_id: SocketId,
    signal_tx: &'a mut EventLoopBus<Signal>,
    endpoints: &'a mut EndpointCollection,
    schedule: &'a mut Schedule,
    timer: &'a mut Timer<Task>,
}

pub struct EndpointEventLoopContext<'a, 'b> {
    socket_id: SocketId,
    endpoint_id: EndpointId,
    signal_tx: &'a mut EventLoopBus<Signal>,
    registrar: &'b mut Registrar,
}

pub struct PipeController {
    socket_id: SocketId,
    endpoint_id: EndpointId,
    pipe: Box<transport::Pipe>,
}

pub struct AcceptorController {
    socket_id: SocketId,
    endpoint_id: EndpointId,
    acceptor: Box<Acceptor>,
}

pub struct EndpointCollection {
    ids: Sequence,
    transports: HashMap<String, Box<Transport + Send>>,
    pipes: HashMap<EndpointId, PipeController>,
    acceptors: HashMap<EndpointId, AcceptorController>,
}

pub struct Schedule {
    ids: Sequence,
    items: HashMap<Scheduled, Timeout>,
}

impl Registrar for EventLoop {
    fn register(&mut self,
                io: &Evented,
                tok: Token,
                interest: Ready,
                opt: PollOpt)
                -> io::Result<()> {
        self.register(io, tok, interest, opt)
    }
    fn reregister(&mut self,
                  io: &Evented,
                  tok: Token,
                  interest: Ready,
                  opt: PollOpt)
                  -> io::Result<()> {
        self.reregister(io, tok, interest, opt)
    }
    fn deregister(&mut self, io: &Evented) -> io::Result<()> {
        self.deregister(io)
    }
}

impl PipeController {
    pub fn ready(&mut self,
                 registrar: &mut Registrar,
                 signal_bus: &mut EventLoopBus<Signal>,
                 events: Ready) {
        let mut ctx = self.create_context(registrar, signal_bus);

        self.pipe.ready(&mut ctx, events);
    }

    pub fn process(&mut self,
                   registrar: &mut Registrar,
                   signal_bus: &mut EventLoopBus<Signal>,
                   cmd: pipe::Command) {
        let mut ctx = self.create_context(registrar, signal_bus);

        match cmd {
            pipe::Command::Open => self.pipe.open(&mut ctx),
            pipe::Command::Close => self.pipe.close(&mut ctx),
            pipe::Command::Send(msg) => self.pipe.send(&mut ctx, msg),
            pipe::Command::Recv => self.pipe.recv(&mut ctx),
        }
    }

    fn create_context<'a, 'b>(&self,
                              registrar: &'b mut Registrar,
                              signal_bus: &'a mut EventLoopBus<Signal>)
                              -> EndpointEventLoopContext<'a, 'b> {
        EndpointEventLoopContext {
            socket_id: self.socket_id,
            endpoint_id: self.endpoint_id,
            signal_tx: signal_bus,
            registrar: registrar,
        }
    }
}

impl AcceptorController {
    pub fn ready(&mut self,
                 registrar: &mut Registrar,
                 signal_bus: &mut EventLoopBus<Signal>,
                 events: Ready) {
        let mut ctx = self.create_context(registrar, signal_bus);
        self.acceptor.ready(&mut ctx, events);
    }

    pub fn process(&mut self,
                   registrar: &mut Registrar,
                   signal_bus: &mut EventLoopBus<Signal>,
                   cmd: pipe::Command) {
        let mut ctx = self.create_context(registrar, signal_bus);

        match cmd {
            pipe::Command::Send(_) => {}
            pipe::Command::Recv => {}
            pipe::Command::Open => self.acceptor.open(&mut ctx),
            pipe::Command::Close => self.acceptor.close(&mut ctx),
        }
    }

    fn create_context<'a, 'b>(&self,
                              registrar: &'b mut Registrar,
                              signal_bus: &'a mut EventLoopBus<Signal>)
                              -> EndpointEventLoopContext<'a, 'b> {
        EndpointEventLoopContext {
            socket_id: self.socket_id,
            endpoint_id: self.endpoint_id,
            signal_tx: signal_bus,
            registrar: registrar,
        }
    }
}

impl EndpointCollection {
    pub fn new(seq: Sequence,
               transports: HashMap<String, Box<Transport + Send>>)
               -> EndpointCollection {
        EndpointCollection {
            ids: seq,
            transports: transports,
            pipes: HashMap::new(),
            acceptors: HashMap::new(),
        }
    }

    fn get_transport(&self, scheme: &str) -> io::Result<&Box<Transport + Send>> {
        self.transports
            .get(scheme)
            .ok_or_else(|| Error::new(ErrorKind::Other, "invalid scheme"))
    }

    pub fn get_pipe_mut(&mut self, eid: EndpointId) -> Option<&mut PipeController> {
        self.pipes.get_mut(&eid)
    }

    pub fn insert_pipe(&mut self, sid: SocketId, pipe: Box<Pipe>) -> EndpointId {
        let eid = EndpointId::from(self.ids.next());

        self.insert_pipe_controller(sid, eid, pipe);

        eid
    }

    fn insert_pipe_controller(&mut self, sid: SocketId, eid: EndpointId, pipe: Box<Pipe>) {
        let controller = PipeController {
            socket_id: sid,
            endpoint_id: eid,
            pipe: pipe,
        };

        self.pipes.insert(eid, controller);
    }

    pub fn remove_pipe(&mut self, eid: EndpointId) {
        self.pipes.remove(&eid);
    }

    pub fn get_acceptor_mut(&mut self, eid: EndpointId) -> Option<&mut AcceptorController> {
        self.acceptors.get_mut(&eid)
    }

    fn insert_acceptor(&mut self, sid: SocketId, acceptor: Box<Acceptor>) -> EndpointId {
        let eid = EndpointId::from(self.ids.next());

        self.insert_acceptor_controller(sid, eid, acceptor);

        eid
    }

    fn insert_acceptor_controller(&mut self,
                                  sid: SocketId,
                                  eid: EndpointId,
                                  acceptor: Box<Acceptor>) {
        let controller = AcceptorController {
            socket_id: sid,
            endpoint_id: eid,
            acceptor: acceptor,
        };

        self.acceptors.insert(eid, controller);
    }
}

impl Schedule {
    pub fn new(seq: Sequence) -> Schedule {
        Schedule {
            ids: seq,
            items: HashMap::new(),
        }
    }
    fn insert(&mut self, handle: Timeout) -> Scheduled {
        let scheduled = Scheduled::from(self.ids.next());
        self.items.insert(scheduled, handle);
        scheduled
    }
    fn remove(&mut self, scheduled: Scheduled) -> Option<Timeout> {
        self.items.remove(&scheduled)
    }
}

impl<'a> SocketEventLoopContext<'a> {
    pub fn new(sid: SocketId,
               tx: &'a mut EventLoopBus<Signal>,
               eps: &'a mut EndpointCollection,
               sched: &'a mut Schedule,
               timer: &'a mut Timer<Task>)
               -> SocketEventLoopContext<'a> {
        SocketEventLoopContext {
            socket_id: sid,
            signal_tx: tx,
            endpoints: eps,
            schedule: sched,
            timer: timer,
        }
    }

    fn send_signal(&mut self, signal: Signal) {
        self.signal_tx.send(signal);
    }

    fn send_pipe_cmd(&mut self, endpoint_id: EndpointId, cmd: pipe::Command) {
        let signal = Signal::PipeCmd(self.socket_id, endpoint_id, cmd);

        self.send_signal(signal);
    }

    fn send_acceptor_cmd(&mut self, endpoint_id: EndpointId, cmd: pipe::Command) {
        let signal = Signal::AcceptorCmd(self.socket_id, endpoint_id, cmd);

        self.send_signal(signal);
    }

    fn send_socket_evt(&mut self, evt: pipe::Event) {
        let signal = Signal::SocketEvt(self.socket_id, evt);

        self.send_signal(signal);
    }

    fn get_transport(&self, scheme: &str) -> io::Result<&Box<Transport + Send>> {
        self.endpoints.get_transport(scheme)
    }

    fn connect(&mut self, tmpl: &EndpointTmpl) -> io::Result<Box<Pipe>> {
        let url = &tmpl.spec.url;
        let index = match url.find("://") {
            Some(x) => x,
            None => return Err(Error::new(ErrorKind::Other, url.to_owned())),
        };

        let (scheme, remainder) = url.split_at(index);
        let addr = &remainder[3..];
        let transport = try!(self.get_transport(scheme));
        let dest = Destination {
            addr: addr,
            pids: tmpl.pids,
            tcp_no_delay: tmpl.spec.desc.tcp_no_delay,
            recv_max_size: tmpl.spec.desc.recv_max_size,
        };

        transport.connect(&dest)
    }

    fn bind(&mut self, tmpl: &EndpointTmpl) -> io::Result<Box<Acceptor>> {
        let url = &tmpl.spec.url;
        let index = match url.find("://") {
            Some(x) => x,
            None => return Err(Error::new(ErrorKind::Other, url.to_owned())),
        };

        let (scheme, remainder) = url.split_at(index);
        let addr = &remainder[3..];
        let transport = try!(self.get_transport(scheme));
        let dest = Destination {
            addr: addr,
            pids: tmpl.pids,
            tcp_no_delay: tmpl.spec.desc.tcp_no_delay,
            recv_max_size: tmpl.spec.desc.recv_max_size,
        };

        transport.bind(&dest)
    }
}

impl<'a> Network for SocketEventLoopContext<'a> {
    fn connect(&mut self, sid: SocketId, tmpl: &EndpointTmpl) -> io::Result<EndpointId> {
        let pipe = try!(self.connect(tmpl));
        let eid = self.endpoints.insert_pipe(sid, pipe);

        Ok(eid)
    }
    fn bind(&mut self, sid: SocketId, tmpl: &EndpointTmpl) -> io::Result<EndpointId> {
        let acceptor = try!(self.bind(tmpl));
        let eid = self.endpoints.insert_acceptor(sid, acceptor);

        Ok(eid)
    }
    fn reconnect(&mut self, sid: SocketId, eid: EndpointId, tmpl: &EndpointTmpl) -> io::Result<()> {
        let pipe = try!(self.connect(tmpl));

        Ok(self.endpoints.insert_pipe_controller(sid, eid, pipe))
    }
    fn rebind(&mut self, sid: SocketId, eid: EndpointId, tmpl: &EndpointTmpl) -> io::Result<()> {
        let acceptor = try!(self.bind(tmpl));

        Ok(self.endpoints.insert_acceptor_controller(sid, eid, acceptor))
    }
    fn open(&mut self, endpoint_id: EndpointId, remote: bool) {
        if remote {
            self.send_pipe_cmd(endpoint_id, pipe::Command::Open);
        } else {
            self.send_acceptor_cmd(endpoint_id, pipe::Command::Open)
        }
    }
    fn close(&mut self, endpoint_id: EndpointId, remote: bool) {
        if remote {
            self.send_pipe_cmd(endpoint_id, pipe::Command::Close);
        } else {
            self.send_acceptor_cmd(endpoint_id, pipe::Command::Close)
        }
    }
    fn send(&mut self, endpoint_id: EndpointId, msg: Rc<Message>) {
        self.send_pipe_cmd(endpoint_id, pipe::Command::Send(msg));
    }
    fn recv(&mut self, endpoint_id: EndpointId) {
        self.send_pipe_cmd(endpoint_id, pipe::Command::Recv);
    }
}

impl<'a> Scheduler for SocketEventLoopContext<'a> {
    fn schedule(&mut self, schedulable: Schedulable, delay: Duration) -> io::Result<Scheduled> {
        let task = Task::Socket(self.socket_id, schedulable);
        let handle = try!(self.timer
            .set_timeout(delay, task)
            .map_err(|_| std::io::Error::new(ErrorKind::Other, "timer error")));
        let scheduled = self.schedule.insert(handle);

        Ok(scheduled)
    }
    fn cancel(&mut self, scheduled: Scheduled) {
        if let Some(handle) = self.schedule.remove(scheduled) {
            self.timer.cancel_timeout(&handle);
        }
    }
}

impl<'a> fmt::Debug for SocketEventLoopContext<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Socket:{:?}", self.socket_id)
    }
}

impl<'a, 'b> EndpointRegistrar for EndpointEventLoopContext<'a, 'b> {
    fn register(&mut self, io: &Evented, interest: Ready, opt: PollOpt) {
        let res = self.registrar.register(io, self.endpoint_id.into(), interest, opt);

        if res.is_err() {
            println!("[{:?}] register failed: {}", self, res.unwrap_err());
        }
    }
    fn reregister(&mut self, io: &Evented, interest: Ready, opt: PollOpt) {
        let res = self.registrar.reregister(io, self.endpoint_id.into(), interest, opt);

        if res.is_err() {
            println!("[{:?}] reregister failed: {}", self, res.unwrap_err());
        }
    }
    fn deregister(&mut self, io: &Evented) {
        let res = self.registrar.deregister(io);

        if res.is_err() {
            println!("[{:?}] deregister failed: {}", self, res.unwrap_err());
        }
    }
}

impl<'a, 'b> fmt::Debug for EndpointEventLoopContext<'a, 'b> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Socket:{:?} Pipe:{:?}", self.socket_id, self.endpoint_id)
    }
}

impl Into<Token> for EndpointId {
    fn into(self) -> Token {
        Token(self.into())
    }
}

impl<'x> Into<Token> for &'x EndpointId {
    fn into(self) -> Token {
        Token(self.into())
    }
}

impl From<Token> for EndpointId {
    fn from(tok: Token) -> EndpointId {
        EndpointId::from(tok.0)
    }
}

impl<'a> dispatcher::Context for SocketEventLoopContext<'a> {
    fn raise(&mut self, evt: pipe::Event) {
        self.send_socket_evt(evt);
    }
}

impl<'a, 'b> endpoint::Context for EndpointEventLoopContext<'a, 'b> {
    fn raise(&mut self, evt: pipe::Event) {
        let signal = Signal::PipeEvt(self.socket_id, self.endpoint_id, evt);
        self.signal_tx.send(signal);
    }
}
// impl<'a, 'b> endpoint::Context for EndpointEventLoopContext<'a, 'b> {
// fn raise(&mut self, evt: pipe::Event) {
// let signal = Signal::AcceptorEvt(self.socket_id, self.endpoint_id, evt);
// self.signal_tx.send(signal);
// }
// }
//